//! Native-client FFI for the backup-destination **management** UI
//! (`docs/goal/ui/backups.md` § Manage backup destinations;
//! `docs/goal/behavior/backup-destinations.md` § State & data shape,
//! ratified 2026-06-14). This is the list read + mutate-and-save surface the
//! 5-client lift needs — over the account plane's per-box `fauna.state.backup`
//! list (`crate::backup_seam`, keyed by the box this connection is bound to):
//! the desktop apps (linux GTK / web wasm) drive `mutate_backup` + the
//! `add_/edit_/remove_backup_destination` mutate helpers directly, but the native UniFFI apps (windows / apple / android)
//! cannot — so this module wraps the **same shared logic** as a handful of free
//! async fns (priority #2/#3; mirrors how `resolve_backup_destination` wraps the
//! enroll-time identity resolution, which these fns call for add/edit).
//!
//! Per priority #2 there is **no** new logic here — each fn sequences shared-Rust
//! calls and returns the freshly-persisted destination list:
//!   * `segment_backup::resolve_destination[_connected]`
//!     (`libs/fauna-sync-engine`) — the `connect` is the `fauna.auth.handshake`
//!     reachability + authorization proof. **add** uses `_connected` and keeps
//!     the session open for the writer-grant registration below; **edit**'s
//!     URL-change re-verification uses the plain form and drops it.
//!   * **add** then hands off to the one shared enroll sequence
//!     `fauna_client_config::enroll_backup_destination` (grant the source nest
//!     this owner's `NestBackupKey`, register the nest-writer grant at the
//!     destination, register the destination with the source nest, then record
//!     the row) — linux and wasm call the same fn, so the sequence lives in
//!     exactly one place;
//!   * **remove** hands off to `fauna_client_config::
//!     deregister_backup_destination` (deregister from the source nest's own
//!     registry, then drop the row);
//!   * **edit** uses the `edit_backup_destination` mutate helper
//!     (`libs/fauna-client-config`) inside `mutate_backup` — the one write door
//!     to the account plane's `fauna.state.backup` list for the box this
//!     connection is bound to; that write is the single atomic decision point
//!     in every case (no destination-side state beyond the writer grant is
//!     mutated synchronously, so a crash mid-enroll/mid-removal is recoverable; the
//!     coordinator reconciles provisioning + deregistration on its next pass).
//!     Mirrors linux `apps/fauna-linux/src/views/backups/destinations.rs`.
//!
//! Gated behind the default-on `backup-destinations` feature so the Go
//! mail-bridge `--no-default-features` FFI build drops it — the bridge is a
//! server with no destination-management UI (same rationale as `mail-admin` /
//! `web-content` / `folders`). The view record crosses only built-in types, so
//! the gate is about dead-code, not a Go-binding incompatibility.

use std::path::PathBuf;
use std::sync::Arc;

use fauna_client_config::{
    BackupStateStore, CustodianEnrollment, ResolvedDestination, attach_folder_to_destination,
    deregister_backup_destination, detach_folder_from_destination, edit_backup_destination,
    enroll_backup_destination, enroll_client_custodian, keep_backup_destination_at_rest,
    list_folder_destinations, load_backup_state, load_backup_state_refiled, mutate_backup,
    read_backup_status,
};
use fauna_core::backup_state::BackupState;
use fauna_core::data::{BackupDestination, DestinationUnattestedMark};

use crate::crypto::secret32;
use crate::nest_client::FfiNestClient;
use crate::segment_backup::FfiBackupDestinationStatus;
use crate::{FfiError, general_err};

/// Sentinel error message a [`backup_destination_edit`] returns when the new URL
/// resolves to a *different* nest identity. The client maps it to the localized
/// `backups.backup_destination_edit_different_nest` string ("Remove this
/// destination and add the new one"). A stable machine token rather than a
/// localized string keeps i18n on the client side (the FFI is locale-agnostic).
pub const EDIT_DIFFERENT_NEST_ERR: &str = "backup-destination-edit-different-nest";

/// One configured backup destination, projected from the bound box's
/// `fauna.state.backup` list for the management UI. Mirrors
/// [`fauna_core::data::BackupDestination`] minus the coordinator-only fields
/// (`destination_actor_pubkey`, `folder_name`, `added_at`) the UI never shows.
///
/// The per-row **live** status (`last_upload_time` / `backlog_count`) is read
/// separately via [`backup_destination_status`], which reads the NEST's own
/// `fauna.backup.status` projection (the 2026-07-24 repoint); the
/// management rows render the not-yet-backed-up baseline ("never" / "0 queued")
/// until uploads exist — uniform with linux (`destinations.rs`).
///
/// The three custodian columns (`kind` / `custodian_device_id` /
/// `capacity_cap_bytes`) landed 2026-08-03 with the client-device kind
/// (`docs/goal/behavior/backup-destinations.md` § Third destination kind). They belong on the
/// *read* projection by this record's own rule — it mirrors
/// [`fauna_core::data::BackupDestination`] minus the fields the UI never shows,
/// and these are precisely fields the UI shows: without `kind` a native shell
/// cannot paint `backup-destination-kind-badge` or decide whether to paint
/// `backup-destination-usage` at all. (Distinct from
/// the `FfiBackupDestination` *coordinator-input* record, which was deleted
/// 2026-08-16 with the client upload driver it fed — this view record is the
/// only destination shape crossing the FFI now.)
#[derive(uniffi::Record, Clone)]
pub struct FfiBackupDestinationView {
    /// Stable client-assigned identifier — the edit/remove target + the key in
    /// the coordinator's sync-state tables.
    pub destination_id: String,
    /// Origin URL of the destination nest (prefills the edit dialog). Empty on
    /// a client-device row — a custodian has no address at all
    /// (`backups.md` § Custodian contract, question 1), which is why the
    /// per-kind fields swap rather than the URL box going blank.
    pub destination_nest_url: String,
    /// User-facing label. `None` ⇒ the client derives it from the URL host via
    /// the shared [`backup_destination_label`] (one source of truth across the
    /// six apps).
    pub display_name: Option<String>,
    /// The kind discriminator — `"nest"` on every pre-existing row (the serde
    /// default), `"client-device"` for a custodian, anything else for a kind a
    /// newer client wrote. **Feed it to [`crate::backup_destination_kind_label`]
    /// rather than string-matching it**: an unrecognised kind renders as itself,
    /// and reaching for a nest-only field on a non-nest row is what the typed
    /// projection exists to stop.
    pub kind: String,
    /// The custodian device's stable sync `device_id`; `None` on every non-
    /// custodian row. A `"client-device"` row with `None` here is a row nothing
    /// can drive — [`every_destination_is_a_client_device`] treats it as *not* a client
    /// device for exactly that reason.
    pub custodian_device_id: Option<String>,
    /// The user-set capacity cap in bytes; `None` = uncapped, which is a real
    /// configuration ("fill the disk") and not a zero.
    pub capacity_cap_bytes: Option<u64>,
    /// Whether this row carries an **open** post-succession review mark — the
    /// row an identity succession carried across that the owner has not yet
    /// kept or removed (`succession-aftermath.md` § Re-key scope →
    /// *Adjudicating what the aftermath carries across*). The shell paints
    /// `backup-destination-unattested-mark` + `backup-destination-keep-button`
    /// on the row exactly when this is set; Keep is [`backup_destination_keep`].
    ///
    /// Resolved HERE, by the shared [`DestinationUnattestedMark::row_is_raised`]
    /// rule, from the same `fauna.state.backup` read the row came from — the
    /// bare [`BackupDestination`] carries no mark at all (the marks ride beside
    /// the list, `BackupState::marks`), so a shell cannot derive it from the
    /// row.
    /// Every export returning rows sets it, so a shell may assign any returned
    /// list wholesale without un-painting another row's mark.
    #[uniffi(default = false)]
    pub unattested: bool,
}

impl From<&BackupDestination> for FfiBackupDestinationView {
    /// The marks-less projection: `unattested` is always `false` here. Exports
    /// resolve it against the state's marks through [`project`] instead.
    fn from(d: &BackupDestination) -> Self {
        Self::of(d, &[])
    }
}

impl FfiBackupDestinationView {
    /// Project one row, resolving its review mark against `marks`
    /// (`BackupState::marks`) by the shared rule.
    fn of(d: &BackupDestination, marks: &[DestinationUnattestedMark]) -> Self {
        Self {
            destination_id: d.destination_id.clone(),
            destination_nest_url: d.destination_nest_url.clone(),
            display_name: d.display_name.clone(),
            kind: d.kind.clone(),
            custodian_device_id: d.custodian_device_id.clone(),
            capacity_cap_bytes: d.capacity_cap_bytes,
            unattested: DestinationUnattestedMark::row_is_raised(marks, d),
        }
    }

    /// This row as the shared custodian-matching input, so the per-kind rules
    /// are read from one place rather than re-derived off [`Self::kind`].
    fn custodian_row(&self) -> fauna_core::data::CustodianRowRef<'_> {
        fauna_core::data::CustodianRowRef {
            destination_id: &self.destination_id,
            kind: &self.kind,
            custodian_device_id: self.custodian_device_id.as_deref(),
            capacity_cap_bytes: self.capacity_cap_bytes,
        }
    }
}

/// `fauna_core::data::every_row_is_a_client_device` → does every configured
/// destination hold its copy on one of the owner's own devices? The
/// `backup-sole-client-destination-warning` predicate
/// (`docs/goal/behavior/backup-destinations.md` § Third destination kind → *Durability +
/// labeling*), over the rows [`backup_destinations_list`] just returned.
///
/// Shared because it is a **policy** answer, not a rendering one: it decides
/// whether a user is told their durability story is weaker than they think.
/// Both arms are the conservative direction and neither is guessable — an empty
/// list is *not* sole-client (it is the empty state, and painting a durability
/// warning on an account with no backup at all is simply false), and a row whose
/// kind this build does not implement counts as *not* a client device (it may
/// well BE the off-site copy the warning would otherwise deny the user has, and
/// crying wolf at someone who is covered is how a standing warning gets tuned
/// out).
#[uniffi::export]
pub fn every_destination_is_a_client_device(destinations: Vec<FfiBackupDestinationView>) -> bool {
    fauna_core::data::every_row_is_a_client_device(
        destinations
            .iter()
            .map(FfiBackupDestinationView::custodian_row),
    )
}

/// `fauna_core::data::custodian_store_is_orphaned` → should this device offer
/// to reclaim its sealed custodian store? The `backup-orphaned-store-row` render
/// rule (`docs/goal/ui/backups.md` § Manage backup destinations → *Reclaim this
/// device's copy*).
///
/// Shared, and emphatically not re-derived per app, because it gates a
/// **destructive** gesture over the owner's only offline copy. Two facts decide
/// it and each app supplies one from a different place: `store_holds_bytes` is
/// this device's own disk — on mobile, [`crate::custodian_store_footprint`]'s
/// `bytes > 0` — while `destinations` is the list the page already loaded.
///
/// ⚠ **It is NOT `custodian_assignment_for(..).is_none()`.** That function
/// answers *"can this device host?"* and returns `None` for **two** rows naming
/// this device (it refuses to guess which cap to honour), so the obvious
/// re-derivation offers to delete a store whose destination rows are still
/// sitting on the user's own Backups page.
///
/// Both refusals are the conservative direction: a device with no sync id of its
/// own is never orphaned (cannot-tell must not paint a delete button), and an
/// empty store offers nothing.
///
/// `this_device_id` is the **stable sync device id**, hex — the same one
/// enrollment recorded in the registry row, and the same one
/// [`backup_destination_enroll_custodian`] takes. Passing a blank string reaches
/// the shared refusal rather than a second message invented here.
#[uniffi::export]
pub fn custodian_store_is_orphaned(
    destinations: Vec<FfiBackupDestinationView>,
    this_device_id: String,
    store_holds_bytes: bool,
) -> bool {
    fauna_core::data::custodian_store_is_orphaned(
        destinations
            .iter()
            .map(FfiBackupDestinationView::custodian_row),
        &this_device_id,
        store_holds_bytes,
    )
}

/// `fauna_core::data::row_is_a_client_device` → is THIS row one of the owner's
/// own devices? The gate on `backup-destination-remove-reclaim-checkbox`, which
/// is offered on client-device rows only.
///
/// The typed rule rather than a raw `kind` compare, for the reason its sibling
/// above is shared: an `Inert` row owns no local store, and a row whose kind
/// this build does not implement is *not* a client device.
#[uniffi::export]
pub fn destination_row_is_a_client_device(destination: FfiBackupDestinationView) -> bool {
    fauna_core::data::row_is_a_client_device(destination.custodian_row())
}

/// The user-facing label for a backup-destination row: the `display_name` when
/// set (non-empty), else the destination URL's host. The native twin of linux
/// `destinations.rs::destination_label`, wrapping the shared
/// `fauna_core::format::backup_destination_label` so windows/apple/android drop
/// their per-app `DestinationLabel` / `label(for:)` glue (priorities #1/#2;
/// `docs/goal/ui/backups.md` § Where logic lives). Locale-agnostic — a host is
/// not localized — so it stays FFI-side rather than going through i18n.
#[uniffi::export]
pub fn backup_destination_label(
    display_name: Option<String>,
    destination_nest_url: String,
) -> String {
    fauna_core::format::backup_destination_label(display_name.as_deref(), &destination_nest_url)
}

/// The source box this connection is bound to — the key of the per-box
/// destination list in `fauna.state.backup`. Unprovable ⇒ an error, never a
/// guess: the reads then show a failure and the writes refuse.
async fn bound_source_nest(nest: &Arc<FfiNestClient>) -> Result<[u8; 32], FfiError> {
    Ok(crate::deployment_seed::bound_nest_id(nest).await?.0)
}

/// One row per enrolled destination — coverage rows (per-folder mirror-set
/// rows sharing a destination_id) are folded away before crossing the FFI
/// boundary, so a management page never renders one destination N+1 times
/// for its N covered folders (`fauna_core::data::
/// distinct_destinations`'s own doc).
///
/// The fold runs FIRST and the mark is resolved on what survives it (the wasm
/// twin `backupDestinationList` states why): the verdict is keyed by
/// `destination_id`, which every coverage row of a destination shares.
fn project(state: &BackupState) -> Vec<FfiBackupDestinationView> {
    project_rows(&state.backup.destinations, &state.marks)
}

fn project_rows(
    destinations: &[BackupDestination],
    marks: &[DestinationUnattestedMark],
) -> Vec<FfiBackupDestinationView> {
    fauna_core::data::distinct_destinations(destinations)
        .iter()
        .map(|d| FfiBackupDestinationView::of(d, marks))
        .collect()
}

/// Project the rows a shared mutation returned — those carry the destination
/// list but not its marks — by re-reading the box's marks. A failed re-read
/// must not fail a mutation that already landed, so it degrades to the
/// marks-less reading ([`FfiBackupDestinationView::from`]'s — nothing raised):
/// narrower, never wrong in a way the next list read does not fix.
async fn project_after_mutation(
    store: &dyn BackupStateStore,
    source_nest: [u8; 32],
    destinations: &[BackupDestination],
) -> Vec<FfiBackupDestinationView> {
    let marks = match load_backup_state(store, source_nest).await {
        Ok(state) => state.marks,
        Err(e) => {
            tracing::warn!(error = %e, "backup destinations: mark re-read failed; no marks projected");
            Vec::new()
        }
    };
    project_rows(destinations, &marks)
}

/// Read the configured backup destinations from the account plane's
/// `fauna.state.backup` list for the box this connection is bound to
/// (re-filing a list still keyed under an older box id). Renders the
/// management rows. `owner_secret` is still validated for the export's
/// stable shape; the plane read itself needs none.
#[fauna_uniffi_async::export]
pub async fn backup_destinations_list(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
) -> Result<Vec<FfiBackupDestinationView>, FfiError> {
    secret32(&owner_secret)?;
    let source_nest = bound_source_nest(&nest).await?;
    let state =
        load_backup_state_refiled(crate::backup_seam().as_ref(), &nest.nest_arc(), source_nest)
            .await
            .map_err(general_err)?;
    Ok(project(&state))
}

/// **Keep** one raised destination — the owner answering "I recognise this"
/// (`succession-aftermath.md` § Re-key scope → *Adjudicating what the aftermath
/// carries across*). Records the verdict at rest and leaves the destination
/// enrolled; Remove stays [`backup_destination_remove`], so no second removal
/// path is minted. The write is the shared
/// [`fauna_client_config::keep_backup_destination_at_rest`] — through the one
/// `mutate_backup` door, so a Keep cannot clobber a concurrent device's
/// adjudication — then the
/// list is re-read, so the returned rows are the at-rest truth. A Keep on a row
/// already answered (or gone) writes nothing and is not an error. The wasm twin
/// is `backupDestinationKeep`.
#[fauna_uniffi_async::export]
pub async fn backup_destination_keep(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    id: String,
) -> Result<Vec<FfiBackupDestinationView>, FfiError> {
    secret32(&owner_secret)?;
    let source_nest = bound_source_nest(&nest).await?;
    let store = crate::backup_seam();
    keep_backup_destination_at_rest(store.as_ref(), source_nest, &id)
        .await
        .map_err(general_err)?;
    let state = load_backup_state(store.as_ref(), source_nest)
        .await
        .map_err(general_err)?;
    Ok(project(&state))
}

/// Resolve a candidate destination's identity (reachability + authorization),
/// then run the shared enroll sequence
/// ([`fauna_client_config::enroll_backup_destination`] — grant the source nest
/// this owner's `NestBackupKey`, then record the destination). Returns the
/// updated destination list. A blank `name` defaults to the destination's handle
/// domain.
///
/// Only the resolve step is native-specific (it builds a `NestClient`); the rest
/// is the one shared sequence linux and web also call.
#[fauna_uniffi_async::export]
pub async fn backup_destination_add(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    url: String,
    name: String,
) -> Result<Vec<FfiBackupDestinationView>, FfiError> {
    let secret = secret32(&owner_secret)?;
    // Resolve identity + verify reachability/authorization BEFORE recording,
    // keeping the authenticated connection open — the shared enroll sequence
    // reuses it to register the nest-writer grant at the destination.
    let (resolved, destination) =
        fauna_sync_engine::segment_backup::resolve_destination_connected(secret, &url)
            .await
            .map_err(general_err)?;
    // The writer the destination authorizes is the id this connection proved.
    // It is also the key of the box's `fauna.state.backup` list the row lands in.
    let source_nest_id = bound_source_nest(&nest).await?;
    let store = crate::backup_seam();
    let destinations = enroll_backup_destination(
        nest.nest_arc(),
        destination,
        store.as_ref(),
        secret,
        ResolvedDestination {
            destination_id: uuid::Uuid::new_v4().to_string(),
            destination_nest_url: url,
            destination_actor_pubkey: resolved.actor_pubkey,
            domain: resolved.domain,
            requested_name: name,
        },
        source_nest_id,
    )
    .await
    .map_err(general_err)?;
    Ok(project_after_mutation(store.as_ref(), source_nest_id, &destinations).await)
}

/// Enroll **this device** as a client custodian — the second add path
/// (`backups.md` § Third destination kind → *Enrollment*), and the native twin
/// of what linux/tui reach by linking `fauna_client_config` directly.
///
/// A **separate verb rather than more arguments** on [`backup_destination_add`],
/// for the reason the shared layer already split them: a custodian has no
/// address, so there is no URL to pass, nothing to resolve, and no destination
/// nest to open a session with. Passing an empty URL into the nest path would
/// reach `resolve_destination_connected` and fail on a network round-trip that
/// should never have been attempted.
///
/// Everything the sequence *decides* — the registry write, the config write,
/// the crash-safety ordering, the blank-name fallback, and the deliberate
/// absence of a `NestBackupKey` grant — belongs to
/// [`fauna_client_config::enroll_client_custodian`]. This supplies only what the
/// **shell** knows, exactly as linux's own `enroll_custodian` does:
///
/// * `custodian_device_id` — **this** device's stable sync device id, hex, the
///   same one its file-sync engines present. The shell must read it rather than
///   mint one: the source nest keys the custodian's status row on it, so a
///   fresh id would project a row nothing drives. A blank id is refused by the
///   shared enroll (never defaulted here), which is why this takes it as a
///   plain `String` and forwards it untouched.
/// * `capacity_cap_bytes` — the kind's only knob, already parsed by the shell
///   through the shared `parse_byte_size` (the `backup-destination-capacity-input`
///   text is a human string; parsing it is not this boundary's job). `None` is
///   **uncapped**, a real choice — never a substituted default, and never a
///   `Some(0)`, which would report cap-reached forever having stored nothing.
///
/// The `destination_id` is minted here, as [`backup_destination_add`] mints its
/// own — one less thing four shells can get wrong.
#[fauna_uniffi_async::export]
pub async fn backup_destination_enroll_custodian(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    custodian_device_id: String,
    name: String,
    capacity_cap_bytes: Option<u64>,
) -> Result<Vec<FfiBackupDestinationView>, FfiError> {
    secret32(&owner_secret)?;
    let source_nest = bound_source_nest(&nest).await?;
    let store = crate::backup_seam();
    let destinations = enroll_client_custodian(
        nest.nest_arc(),
        store.as_ref(),
        source_nest,
        CustodianEnrollment {
            destination_id: uuid::Uuid::new_v4().to_string(),
            custodian_device_id,
            display_name: name,
            capacity_cap_bytes,
        },
    )
    .await
    .map_err(general_err)?;
    Ok(project_after_mutation(store.as_ref(), source_nest, &destinations).await)
}

/// Rename a destination and/or change its URL. A URL change must point at the
/// **same** nest identity (a different nest is remove + re-add, signaled by the
/// [`EDIT_DIFFERENT_NEST_ERR`] sentinel) — mirrors linux `edit_destination`.
#[fauna_uniffi_async::export]
pub async fn backup_destination_edit(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    id: String,
    url: String,
    name: String,
) -> Result<Vec<FfiBackupDestinationView>, FfiError> {
    let secret = secret32(&owner_secret)?;
    let source_nest = bound_source_nest(&nest).await?;
    let store = crate::backup_seam();
    let state = load_backup_state(store.as_ref(), source_nest)
        .await
        .map_err(general_err)?;

    // Re-resolve only when editing a known destination whose URL changed
    // (renaming an offline destination must still work); reject if the new URL
    // points at a different nest identity.
    let url_changed = state
        .backup
        .destinations
        .iter()
        .find(|d| d.destination_id == id)
        .filter(|stored| stored.destination_nest_url != url)
        .cloned();
    if let Some(stored) = url_changed {
        let resolved = fauna_sync_engine::segment_backup::resolve_destination(secret, &url)
            .await
            .map_err(general_err)?;
        if resolved.actor_pubkey != stored.destination_actor_pubkey {
            return Err(FfiError::General {
                msg: EDIT_DIFFERENT_NEST_ERR.to_string(),
            });
        }
    }

    // Through the one write door, which re-reads the box's list and applies the
    // edit to THAT (the same-nest check above is deliberately validated
    // against the earlier read — see `segment_backup::edit_destination` for
    // why that is safe).
    let display_name = if name.is_empty() { None } else { Some(name) };
    let (state, ()) = mutate_backup(store.as_ref(), source_nest, |st| {
        edit_backup_destination(st, &id, display_name, url);
    })
    .await
    .map_err(general_err)?;
    Ok(project(&state))
}

/// Drop a destination: deregister it from the source nest's own registry,
/// then drop the row from the box's `fauna.state.backup` list (the single
/// atomic decision point). No
/// *destination*-side call — the coordinator reconciles the offsite
/// deregistration on its next pass (`backups.md` § Remove), and the
/// destination-side nest-writer grant is not revoked (a distinct trust-facet
/// action). Crash-recoverable; mirrors linux `remove_destination`.
#[fauna_uniffi_async::export]
pub async fn backup_destination_remove(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    id: String,
) -> Result<Vec<FfiBackupDestinationView>, FfiError> {
    secret32(&owner_secret)?;
    let source_nest = bound_source_nest(&nest).await?;
    let store = crate::backup_seam();
    let destinations =
        deregister_backup_destination(nest.nest_arc(), store.as_ref(), source_nest, &id)
            .await
            .map_err(general_err)?;
    Ok(project_after_mutation(store.as_ref(), source_nest, &destinations).await)
}

/// Read the per-destination backup status
/// (`backup-destination-last-upload-time` / `-backlog-count`) for the configured
/// destinations, one [`FfiBackupDestinationStatus`] per row (`backups.md`
/// § Per-destination status read).
///
/// **Repointed 2026-07-24 (slice-4 leg (d)):** this now reads the **nest's**
/// `fauna.backup.status` projection via the shared
/// `fauna_client_config::read_backup_status`, rather than computing the rows
/// client-side from a local upload coordinator (deleted 2026-08-17). The projection is advanced by
/// the nest's own in-process coordinator, so it is live with every app asleep
/// — which is what makes the page truthful for web and mobile users, who have no
/// local coordinator to ask. Every app now shares one read and one row shape
/// (priority #1/#2); the old source-side computation, its `(device_id, data_dir)`
/// canonical-path contract, and the desktop "fold the read onto the live upload
/// driver handle" optimizations all retire with it.
///
/// The shared read also **heals a pre-enrollment owner** in passing (a
/// destination added before the nest-side enroll calls landed 2026-07-24 has a
/// list row but no grant, so the nest would report nothing) — see
/// `fauna_client_config::read_backup_status` for the sequence and for why the
/// destination-side writer grant is deliberately not part of that heal.
///
/// Zero destinations ⇒ an empty vec: the nest reports no rows, and the heal
/// declines to enroll an owner who has configured nothing. The rows render only
/// when ≥1 destination is configured.
///
/// # Errors
///
/// - `FfiError::General` if `owner_secret` is not 32 bytes.
/// - `FfiError::General` carrying the bound-nest, status-read, list-read or
///   heal error chain. The client degrades a failure to the not-yet-backed-up baseline
///   ("never" / "0 queued"), so a transient socket hiccup on page mount shows no
///   scary error.
#[fauna_uniffi_async::export]
pub async fn backup_destination_status(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
) -> Result<Vec<FfiBackupDestinationStatus>, FfiError> {
    let secret = secret32(&owner_secret)?;
    let source_nest = bound_source_nest(&nest).await?;
    let reply = read_backup_status(
        nest.nest_arc(),
        crate::backup_seam().as_ref(),
        secret,
        source_nest,
    )
    .await
    .map_err(general_err)?;
    Ok(reply
        .destinations
        .into_iter()
        .map(FfiBackupDestinationStatus::from)
        .collect())
}

// ── Destination places (backup-destinations.md § Ordinary-folder coverage) ──
//
// The folders page's per-folder *Destination places* section: attach/detach an
// enrolled backup destination to ONE ordinary folder. android is a non-linking
// consumer of `fauna_client_config::{list_folder_destinations,
// attach_folder_to_destination, detach_folder_from_destination}` (linux/tui
// link the crate directly, web reaches the same three over its own wasm face —
// the uncalled-export rule). Each mutation re-reads the folder's places from
// the nest afterwards and returns THAT, never an optimistic flip — the same
// non-optimistic contract linux's `attach_folder_destination` /
// `detach_folder_destination` follow.

/// FFI mirror of [`fauna_client_config::FolderDestinationPlace`] — one
/// enrolled backup destination, marked attached-or-not for ONE ordinary
/// folder.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiFolderDestinationPlace {
    /// [`FfiBackupDestinationView::destination_id`] — the attach/detach kinds'
    /// key and the attach select's round-trip value.
    pub destination_id: String,
    /// The user-facing label: the list row's display name, falling back to
    /// the id when the box's list could not be read.
    pub label: String,
    /// Whether this folder is attached to the destination.
    pub attached: bool,
    /// The destination-side `__folder/<hex>/<id>` set name, present on
    /// attached rows — the detach sequence's own key.
    pub folder_set: Option<String>,
}

impl From<fauna_client_config::FolderDestinationPlace> for FfiFolderDestinationPlace {
    fn from(p: fauna_client_config::FolderDestinationPlace) -> Self {
        Self {
            destination_id: p.destination_id,
            label: p.label,
            attached: p.attached,
            folder_set: p.folder_set,
        }
    }
}

fn project_places(
    places: Vec<fauna_client_config::FolderDestinationPlace>,
) -> Vec<FfiFolderDestinationPlace> {
    places
        .into_iter()
        .map(FfiFolderDestinationPlace::from)
        .collect()
}

/// `fauna.backup.destination.list` joined with the box's `fauna.state.backup`
/// display names, for ONE folder — the section's lazy-on-first-expand read.
#[fauna_uniffi_async::export]
pub async fn folder_destinations_list(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    folder_id: i64,
) -> Result<Vec<FfiFolderDestinationPlace>, FfiError> {
    secret32(&owner_secret)?;
    let source_nest = bound_source_nest(&nest).await?;
    let places = list_folder_destinations(
        nest.nest_arc(),
        crate::backup_seam().as_ref(),
        source_nest,
        folder_id,
    )
    .await
    .map_err(general_err)?;
    Ok(project_places(places))
}

/// Attach `folder_id` to `destination_id`
/// ([`fauna_client_config::attach_folder_to_destination`]), then re-read this
/// folder's places so the caller repaints from the nest's own answer. Mirrors
/// [`folder_destination_detach`]. A re-read failure after a successful attach
/// degrades to an empty list rather than erroring — the mutation already
/// landed, so that isn't the caller's error to see.
#[fauna_uniffi_async::export]
pub async fn folder_destination_attach(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    folder_id: i64,
    destination_id: String,
) -> Result<Vec<FfiFolderDestinationPlace>, FfiError> {
    secret32(&owner_secret)?;
    let source_nest = bound_source_nest(&nest).await?;
    let store = crate::backup_seam();
    attach_folder_to_destination(
        nest.nest_arc(),
        store.as_ref(),
        source_nest,
        &destination_id,
        folder_id,
    )
    .await
    .map_err(general_err)?;
    let places = list_folder_destinations(nest.nest_arc(), store.as_ref(), source_nest, folder_id)
        .await
        .unwrap_or_default();
    Ok(project_places(places))
}

/// Detach `folder_id` from `destination_id`
/// ([`fauna_client_config::detach_folder_from_destination`]), then re-read
/// this folder's places. `folder_set` is the attached row's own
/// `__folder/<hex>/<id>` name, carried by the [`FfiFolderDestinationPlace`]
/// the detach button's row was built from — never re-derived here.
#[fauna_uniffi_async::export]
pub async fn folder_destination_detach(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    folder_id: i64,
    destination_id: String,
    folder_set: String,
) -> Result<Vec<FfiFolderDestinationPlace>, FfiError> {
    secret32(&owner_secret)?;
    let source_nest = bound_source_nest(&nest).await?;
    let store = crate::backup_seam();
    detach_folder_from_destination(
        nest.nest_arc(),
        store.as_ref(),
        source_nest,
        &destination_id,
        folder_id,
        &folder_set,
    )
    .await
    .map_err(general_err)?;
    let places = list_folder_destinations(nest.nest_arc(), store.as_ref(), source_nest, folder_id)
        .await
        .unwrap_or_default();
    Ok(project_places(places))
}

// ── the client-side audit surface ────────────────────────────────────────────
//
// `docs/goal/ui/backups.md` § Audit-alert surface. linux, tui and web reach
// `fauna_client_backup::audit::run_audit_pass` as *library* code — natively in
// Rust, or through wasm. The four UniFFI shells (windows / macos / ios /
// android) cannot, and that missing face — not any rendering work — is the one
// prerequisite that section names for all four. It is built **once**, here.
//
// The shape is prescribed by that section and mirrors the wasm twin
// `backupAuditRunPass` (`libs/fauna-wasm/src/rpc.rs`): **one** encapsulating
// call that builds all three seams internally and hands back already-resolved
// rows carrying `alert_reason` rather than the raw `AuditVerdict`, so which
// verdicts are loud cannot be re-derived on the far side of the boundary.

/// One destination's audit picture, resolved for rendering.
///
/// The FFI twin of `WasmDestinationAuditRow`, field-for-field (priority #1/#3),
/// projected from `fauna_client_backup::audit::DestinationAuditRecord`.
/// Deliberately **not** a wrapper over the record: the record carries the whole
/// `AuditVerdict`, and a shell handed that would be one `match` away from
/// deciding for itself which states deserve a banner — the drift
/// `DestinationAuditRecord::alert_reason` exists to make impossible.
#[cfg(feature = "backup-audit")]
#[derive(uniffi::Record, Clone)]
pub struct FfiDestinationAuditRow {
    pub destination_id: String,
    /// Unix **seconds** of the last *passed* audit; `None` = never passed, which
    /// renders "never" and is deliberately not an alert. Narrowed from the
    /// record's `i64` exactly as linux and web do — a negative stamp is not a
    /// time this client could have written.
    pub last_passed_at: Option<u64>,
    /// The standing banner reason, or `None` for a healthy destination. Feed it
    /// to [`crate::backup_audit_alert_label`] for the banner text.
    pub alert_reason: Option<fauna_core::format::BackupAuditAlertReason>,
    /// Every banner reason standing at the pass's `now` — the shell's single
    /// `backup-audit-alert` answer (`DestinationAuditRecord::alert_reasons`): the
    /// standing verdict's reason, then at most one
    /// `BackupAuditAlertReason::SourceRegressed`. Paint every entry; feed each to
    /// [`crate::backup_audit_alert_label`] and treat the reason as opaque.
    /// [`Self::alert_reason`] stays for shells not yet moved.
    pub alert_reasons: Vec<fauna_core::format::BackupAuditAlertReason>,
}

/// Run one client-side backup **audit** pass and hand back the full
/// per-destination picture (`backups.md` § Audit-alert surface).
///
/// This implements **no** audit logic. `run_audit_pass` loads this device's
/// state, audits everything due, merges the outcomes over what was already
/// known, persists, and returns one record per configured destination — merge,
/// debounce and verdict all shared. A shell renders the returned vector in
/// order (it is ordered like the destination list, so banner order and row order
/// agree without the shell sorting anything) and implements none of it.
///
/// The three seams are built here, each the native one every other native app
/// already uses:
///
/// * the **connector** — `fauna_client_pair::native_backup_destination_connector`,
///   so the client opens its **own** authenticated session to each *destination*,
///   never a read through the source nest;
/// * the **inclusion source** — `fauna_client_pair::native_backup_inclusion_source`,
///   so sampled records are fetched from the party being audited and opened under
///   the owner's derived `NestBackupKey`, with the covered-folder mirror plane's
///   population anchored in this device's synced replica
///   (`fauna_sync_engine::segment_backup::bound_replica_folder_index` over
///   `sync_state_dir` — the dir the shell's `FfiSyncEngineHost` was built on;
///   `None`, the default, is the declared absence for a shell that holds no
///   replica or has not wired it yet);
/// * the **store** — `fauna_client_backup::native_store::FileAuditStateStore` at
///   `state_path`.
///
/// The destination set audited is the client's **own pinned** list — the
/// bound box's `fauna.state.backup` destinations — not a nest-side list: a destination the
/// source nest has "forgotten" must still be audited, and must still alert.
///
/// `now` comes from `fauna_client_backup::audit_clock::now_secs` — real time plus
/// the e2e offset, which is zero in every real run.
///
/// `own_custodian` is this device's own custodian store as the shell's store
/// read returned it ([`crate::custodian_store_footprint`] on a phone, the
/// agent provisioner's `custodian_store` on a desktop) with this device's sync
/// id: the pass folds its standing source regressions into the row that
/// assigns this device, the fifth banner reason. `None`, the default, is "no
/// store was read" — the row's record stands as the last pass left it.
///
/// ⚠ **`state_path` must be actor-scoped** on any shell where an account switch
/// happens in-process (`backups.md` § Audit-alert surface: "the store key itself
/// is actor-scoped too"). Two accounts sharing one path would let one account's
/// observation high-water silently suppress the other's freshness failures —
/// which fails *quietly*, in the safe-looking direction.
///
/// # Errors
///
/// - `FfiError::General` if `owner_secret` is not 32 bytes.
/// - `FfiError::General` carrying the bound-nest or list-read error chain. A destination that
///   cannot be reached is **not** an error — it is an `Unreachable` verdict the
///   shared loop deliberately keeps quiet (the laptop-on-a-plane case), so the
///   only failure that surfaces here is one that leaves the pass with nothing to
///   audit at all.
#[cfg(feature = "backup-audit")]
#[fauna_uniffi_async::export(default(sync_state_dir = None, own_custodian = None))]
pub async fn backup_audit_run_pass(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    state_path: String,
    sync_state_dir: Option<String>,
    own_custodian: Option<crate::FfiOwnCustodianStore>,
) -> Result<Vec<FfiDestinationAuditRow>, FfiError> {
    let secret = secret32(&owner_secret)?;
    let source_nest = bound_source_nest(&nest).await?;
    let state = load_backup_state(crate::backup_seam().as_ref(), source_nest)
        .await
        .map_err(general_err)?;

    let connector = fauna_client_pair::native_backup_destination_connector(secret);
    // The covered-folder mirror plane's population anchor is this device's
    // synced replica of each covered folder — the sync engine's
    // `ReplicaFolderIndex` over `sync_state_dir` (the dir the shell's
    // `FfiSyncEngineHost` keeps its `fsid-<ref>.db` files in) and the nest
    // `nest` is bound to. A shell that passes `None` declares the absence: that
    // plane keeps hash-verified presence over the destination's list.
    let folder_index = fauna_sync_engine::segment_backup::bound_replica_folder_index(
        &nest.nest_arc(),
        sync_state_dir.map(PathBuf::from),
    )
    .await;
    let inclusion = fauna_client_pair::native_backup_inclusion_source(secret, folder_index);
    let store =
        fauna_client_backup::native_store::FileAuditStateStore::at(PathBuf::from(state_path));

    // The owner's own nest — the connection `nest` is — dialled by the pass
    // only to settle a ledger regression a destination served (the generation
    // pin, `fauna_client_backup::audit::SourceLedgerVouch`).
    let source_nest_url = nest.nest_arc().nest_url();
    let now = fauna_client_backup::audit_clock::now_secs();
    let own_custodian = own_custodian.map(|own| fauna_client_backup::audit::OwnCustodianStore {
        device_id: own.device_id,
        source_regressions: own
            .source_regressions
            .into_iter()
            .map(|r| fauna_client_backup::audit::StoreSourceRegression {
                ledger: r.ledger,
                held: r.held,
                served: r.served,
                observed_at: r.observed_at,
            })
            .collect(),
    });
    let pass = fauna_client_backup::audit::run_audit_pass(
        connector.as_ref(),
        inclusion.as_ref(),
        &store,
        &state.backup.destinations,
        &source_nest_url,
        &source_nest,
        own_custodian.as_ref(),
        now,
    )
    .await;
    for degradation in &pass.degradations {
        tracing::warn!("backup audit: {degradation}");
    }

    Ok(pass.records.iter().map(|r| audit_row(r, now)).collect())
}

/// The directory this machine's local sync **agent** keeps `actor_id_hex`'s
/// per-set replica DBs (`fsid-<ref>.db`) in — the `sync_state_dir` a shell whose
/// bound folders the agent hosts (windows: no in-app engine) hands
/// [`backup_audit_run_pass`]. A thin face over
/// `fauna_sync_engine::segment_backup::local_agent_state_dir`, the one derivation
/// linux and tui call directly, so no shell re-derives the agent's layout (the
/// per-OS base, and on windows the test-build harness data-dir override).
#[cfg(feature = "backup-audit")]
#[uniffi::export]
pub fn backup_audit_local_agent_state_dir(actor_id_hex: String) -> String {
    fauna_sync_engine::segment_backup::local_agent_state_dir(&actor_id_hex)
        .to_string_lossy()
        .into_owned()
}

/// Project one persisted record to its render row. Split out so the
/// verdict-never-crosses rule is unit-testable without a nest. `now` is the
/// pass's own clock, so a recovery window's countdown agrees with the pass.
#[cfg(feature = "backup-audit")]
fn audit_row(
    record: &fauna_client_backup::audit::DestinationAuditRecord,
    now: i64,
) -> FfiDestinationAuditRow {
    FfiDestinationAuditRow {
        destination_id: record.state.destination_id.clone(),
        last_passed_at: record
            .state
            .last_passed_at
            .and_then(|s| u64::try_from(s).ok()),
        alert_reason: record.alert_reason(),
        alert_reasons: record.alert_reasons(now),
    }
}

/// Feed the audit's observation high-water: "this client has displayed activity
/// stamped `last_activity_ms`".
///
/// **This is the load-bearing half, not an afterthought.** Freshness compares
/// what the *destination* holds against what this client itself knows exists, and
/// the client must not learn the latter from the party being audited — a source
/// nest answering "nothing new" would otherwise make freshness unfailable
/// forever. A shell that renders the two elements but never calls this ships a
/// **permanently-passing** audit, and nothing about it looks broken
/// (`backups.md` § Audit-alert surface). Call it from the conversation list's
/// render, as linux, tui and web all do — the one place the client shows the user
/// what it knows about the nest-originated kinds.
///
/// `last_activity_ms` is epoch **milliseconds**, as every app's thread summary
/// carries it; the seconds conversion the store wants is owned once by
/// `fauna_client_backup::audit::activity_secs`, which this face reaches through
/// the shared fold below. A missing `/1000` reads ~50 years ahead and would make
/// every freshness comparison meaningless while still "persisting something" —
/// so it is not left to any shell to get right. (Until 2026-08-22 this comment
/// claimed the conversion happened "here, once" while three other shells each
/// wrote their own `/1000`; the fold now genuinely has one owner.)
///
/// Returns whether anything was persisted, so a shell can keep its own in-memory
/// high-water and skip the call on the common no-op (the shared
/// `observe_local_record` is monotonic, so a repeat render writes nothing).
/// Never errors: a missed observation costs at most a weaker freshness comparison
/// until the next render, and there is nothing a user could do about it.
///
/// ⚠ `state_path` carries the same actor-scoping requirement as
/// [`backup_audit_run_pass`] — and it bites hardest here, since this is the value
/// that would leak across accounts.
#[cfg(feature = "backup-audit")]
#[uniffi::export]
pub fn backup_audit_observe(state_path: String, last_activity_ms: i64) -> bool {
    let store =
        fauna_client_backup::native_store::FileAuditStateStore::at(PathBuf::from(state_path));
    fauna_client_backup::audit::observe_thread_activity(&store, last_activity_ms).persisted
}

/// Set the audit clock's e2e offset
/// (`fauna_client_backup::audit_clock::set_clock_offset_secs`) — the
/// `backup_audit_run_now` agent command's mechanism (testing.md convention 14:
/// a staleness proof moves the clock, never sleeps out the real 48h+ window).
/// Backed by `fauna_client_backup`'s own
/// `#[cfg(any(debug_assertions, feature = "e2e-agent"))]` gate (a no-op in a
/// real release build); this FFI face is additionally gated on `test-helpers`
/// so the symbol itself is absent from a release binding (testing.md
/// convention 15). Mirrors [`crate::atproto_settings::set_delegation_clock_offset_secs`].
///
/// ⚠ Process-wide, and nothing auto-resets it — a test that leaves an offset
/// behind silently lapses the very next audit pass the process runs.
#[cfg(all(feature = "backup-audit", feature = "test-helpers"))]
#[uniffi::export]
pub fn set_backup_audit_clock_offset_secs(offset_secs: i64) {
    fauna_client_backup::audit_clock::set_clock_offset_secs(offset_secs);
}

#[cfg(test)]
mod tests {
    use fauna_client_config::DEFAULT_BACKUP_FOLDER as DEFAULT_FOLDER;

    use super::*;

    /// The rows alone, no mark plane — what every projection test below that is
    /// not about the review mark needs.
    fn project(rows: &[BackupDestination]) -> Vec<FfiBackupDestinationView> {
        project_rows(rows, &[])
    }

    fn dest(id: &str, url: &str, name: Option<&str>) -> BackupDestination {
        BackupDestination {
            destination_id: id.to_string(),
            destination_nest_url: url.to_string(),
            destination_actor_pubkey: [7u8; 32],
            folder_name: DEFAULT_FOLDER.to_string(),
            added_at: 123,
            display_name: name.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    fn custodian(id: &str, device_id: Option<&str>, cap: Option<u64>) -> BackupDestination {
        BackupDestination {
            destination_id: id.to_string(),
            kind: fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE.to_string(),
            custodian_device_id: device_id.map(str::to_string),
            capacity_cap_bytes: cap,
            ..Default::default()
        }
    }

    #[test]
    fn view_projects_ui_fields_only() {
        let rows = vec![
            dest("id-a", "https://a.example.com", Some("Aunt's nest")),
            dest("id-b", "https://b.example.com", None),
        ];
        let views = project_rows(&rows, &[]);
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].destination_id, "id-a");
        assert_eq!(views[0].destination_nest_url, "https://a.example.com");
        assert_eq!(views[0].display_name.as_deref(), Some("Aunt's nest"));
        // Blank label rides as None — the client derives it from the URL host.
        assert_eq!(views[1].display_name, None);
        // A pre-discriminator row reads as what it is, so the badge a native
        // shell paints for an untouched destination says "nest".
        assert_eq!(views[0].kind, "nest");
        assert_eq!(views[0].custodian_device_id, None);
        assert_eq!(views[0].capacity_cap_bytes, None);
    }

    /// `attach_backup_destination_folder` clones the enrolled row
    /// per covered folder, so a destination with N attached folders has N+1
    /// rows sharing one `destination_id`. The projection must still surface
    /// exactly one view per destination, or every native shell renders a
    /// destination once per folder it covers.
    #[test]
    fn covered_folders_project_into_one_view_per_destination() {
        let enrolled = dest("id-a", "https://a.example.com", Some("Aunt's nest"));
        let covered_a = BackupDestination {
            folder_name: "__folder/deadbeef/1".into(),
            ..enrolled.clone()
        };
        let covered_b = BackupDestination {
            folder_name: "__folder/deadbeef/2".into(),
            ..enrolled.clone()
        };
        let views = project_rows(&[enrolled, covered_a, covered_b], &[]);
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].destination_id, "id-a");
    }

    /// `unattested` is resolved from the box's marks (`BackupState::marks`),
    /// not the row: an
    /// open mark raises the row, a Keep recorded at rest (by this or a sibling
    /// device) lowers it, and an ordinary row never carries it. A shell holding
    /// only `BackupDestination` cannot tell, which is why the field exists on
    /// this record.
    #[test]
    fn the_view_resolves_the_review_mark_from_the_state_marks() {
        use fauna_core::data::UnattestedVerdict;
        use fauna_core::identity::ActorId;
        let predecessor = ActorId([3u8; 32]);
        let stamped = |id: &str| dest(id, "https://a.example.com", None);
        let mark = |id: &str, verdict| DestinationUnattestedMark {
            destination_id: id.into(),
            predecessor,
            verdict,
        };
        let rows = [
            stamped("open"),
            stamped("kept"),
            dest("plain", "https://b.example.com", None),
        ];
        let marks = [
            mark("open", UnattestedVerdict::Open),
            mark("kept", UnattestedVerdict::Kept),
        ];
        let views = project_rows(&rows, &marks);
        let raised = |id: &str| {
            views
                .iter()
                .find(|v| v.destination_id == id)
                .unwrap()
                .unattested
        };
        assert!(raised("open"));
        assert!(!raised("kept"), "a Keep at rest lowers the row");
        assert!(!raised("plain"));
        // With no marks at all nothing is raised: the mark plane is the only
        // authority since the row's legacy stamp was retired.
        assert!(project_rows(&rows, &[]).iter().all(|v| !v.unattested));
    }

    /// The three custodian columns must survive the projection, or no native
    /// shell can paint `backup-destination-kind-badge` /
    /// `backup-destination-usage` at all — they are the only channel by which
    /// windows/macos/ios/android learn a row is a client device.
    #[test]
    fn the_view_carries_the_custodian_columns() {
        let views = project_rows(&[custodian("mine", Some("dev-a"), Some(50 << 30))], &[]);
        assert_eq!(views[0].kind, "client-device");
        assert_eq!(views[0].custodian_device_id.as_deref(), Some("dev-a"));
        assert_eq!(views[0].capacity_cap_bytes, Some(50 << 30));
    }

    /// The sole-client warning over the rows a shell already holds. The Inert
    /// arm is the load-bearing one: an unrecognised kind counts as *not* a
    /// client device, because it may well BE the off-site copy the warning
    /// would otherwise tell the user they do not have.
    #[test]
    fn every_destination_is_a_client_device_reads_the_shared_rule_not_the_kind_string() {
        let mine = project(&[custodian("mine", Some("dev-a"), None)]);
        assert!(every_destination_is_a_client_device(mine.clone()));

        let with_offsite = project(&[
            custodian("mine", Some("dev-a"), None),
            dest("friend", "https://friend.example.com", None),
        ]);
        assert!(!every_destination_is_a_client_device(with_offsite));

        // Empty is the empty state, not a sole-client one.
        assert!(!every_destination_is_a_client_device(vec![]));

        // A client-device row with no device id is a row nothing can drive, so
        // it is no evidence of a copy on this device either.
        let half_written = project(&[
            custodian("mine", Some("dev-a"), None),
            custodian("half-written", None, None),
        ]);
        assert!(!every_destination_is_a_client_device(half_written));

        // A kind this build does not implement.
        let future = project(&[BackupDestination {
            destination_id: "future".into(),
            kind: "s3".into(),
            ..Default::default()
        }]);
        assert!(!every_destination_is_a_client_device(future));
    }

    /// The status projection must carry the two custodian columns, or no native
    /// shell can render `backup-destination-usage` **honestly**.
    ///
    /// The failure this pins is not "the row is blank" — it is far worse. A
    /// shell handed only `held_bytes` (or neither) has exactly one way left to
    /// answer "is this custodian at its cap": infer it from `held >= cap`. That
    /// inference is wrong in the harmful direction, because a pull pass that
    /// stops at its cap ends *below* the cap — so a backup that has silently
    /// stopped advancing renders as healthy, with room to spare. The whole
    /// reason `cap_state` exists as a distinct wire field is that it is **not**
    /// derivable from the other two, and the whole reason it must cross this
    /// boundary is that windows/apple/android have no other source for it.
    #[test]
    fn the_status_projection_carries_held_bytes_and_cap_state() {
        use fauna_protocol::backup::{BackupDestinationStatusItem, CAP_STATE_REACHED};

        let capped: FfiBackupDestinationStatus = BackupDestinationStatusItem {
            destination_id: "mine".into(),
            backlog_count: 3,
            // Deliberately *below* the cap while reporting cap-reached — the
            // exact pair a `held >= cap` inference gets wrong.
            held_bytes: Some(40 << 30),
            cap_state: Some(CAP_STATE_REACHED.to_string()),
            ..Default::default()
        }
        .into();
        assert_eq!(capped.held_bytes, Some(40 << 30));
        assert_eq!(capped.cap_state.as_deref(), Some(CAP_STATE_REACHED));

        // A nest row carries neither, and `held_bytes: None` is "never checked
        // in" rather than "holds zero bytes" — the shared label's own two arms.
        let nest_row: FfiBackupDestinationStatus = BackupDestinationStatusItem {
            destination_id: "friend".into(),
            last_upload_time: Some(1_700_000_000),
            ..Default::default()
        }
        .into();
        assert_eq!(nest_row.held_bytes, None);
        assert_eq!(nest_row.cap_state, None);
    }

    /// The same rule as the pair above, for the pair that was dropped for
    /// seventeen days after it: `audit_state` + `last_audit_passed_at` are the
    /// ONLY way a non-linking shell learns whether a client-device custodian's
    /// own copy still verifies (`backup-destinations.md` § Custodian contract,
    /// question 4 — the custodian self-audits and its check-in feeds the nest
    /// projection). A projection that drops them leaves windows/apple/android
    /// no answer at all, which is not the same as a healthy answer.
    ///
    /// The stamp is carried **beside** the verdict, never instead of it: a
    /// failing self-audit deliberately leaves `last_audit_passed_at` at the
    /// previous pass, so the stamp alone renders a freshly-rotted custodian as
    /// just-verified.
    #[test]
    fn the_status_projection_carries_the_custodian_self_audit_verdict() {
        use fauna_core::data::{AUDIT_STATE_FAILED, AUDIT_STATE_OK};
        use fauna_protocol::backup::BackupDestinationStatusItem;

        let failing: FfiBackupDestinationStatus = BackupDestinationStatusItem {
            destination_id: "mine".into(),
            audit_state: Some(AUDIT_STATE_FAILED.to_string()),
            // Stale on purpose: the last time it PASSED, not the last time it ran.
            last_audit_passed_at: Some(1_700_000_000),
            ..Default::default()
        }
        .into();
        assert_eq!(failing.audit_state.as_deref(), Some(AUDIT_STATE_FAILED));
        assert_eq!(failing.last_audit_passed_at, Some(1_700_000_000));

        let passing: FfiBackupDestinationStatus = BackupDestinationStatusItem {
            destination_id: "mine".into(),
            audit_state: Some(AUDIT_STATE_OK.to_string()),
            last_audit_passed_at: Some(1_700_000_500),
            ..Default::default()
        }
        .into();
        assert_eq!(passing.audit_state.as_deref(), Some(AUDIT_STATE_OK));

        // Absence crosses as absence — a custodian that has never audited
        // must reach the far side as *unknown*.
        let silent: FfiBackupDestinationStatus = BackupDestinationStatusItem {
            destination_id: "friend".into(),
            ..Default::default()
        }
        .into();
        assert_eq!(silent.audit_state, None);
        assert_eq!(silent.last_audit_passed_at, None);
    }

    /// The audit row must carry the *banner reason*, never the verdict — the
    /// whole point of the projection (`backups.md` § Audit-alert surface: the
    /// loud/quiet decision "cannot be re-derived on the far side of the
    /// boundary"). `Unreachable` is the case that proves it: a shell handed the
    /// verdict would very plausibly alert on it, and the shared loop
    /// deliberately keeps it quiet.
    #[cfg(feature = "backup-audit")]
    #[test]
    fn audit_row_carries_the_banner_reason_not_the_verdict() {
        use fauna_client_backup::audit::{
            AuditVerdict, DestinationAuditRecord, DestinationAuditState,
        };
        use fauna_core::format::BackupAuditAlertReason;

        let audit_row = |r: &DestinationAuditRecord| super::audit_row(r, 1_800_000_000);
        let record =
            |verdict: Option<AuditVerdict>, last_passed_at: Option<i64>| DestinationAuditRecord {
                state: DestinationAuditState {
                    destination_id: "id-a".to_string(),
                    last_passed_at,
                    ..Default::default()
                },
                verdict,
            };

        // Never audited → "never", and explicitly not an alert.
        let never = audit_row(&DestinationAuditRecord::never("id-a"));
        assert_eq!(never.destination_id, "id-a");
        assert_eq!(never.last_passed_at, None);
        assert!(never.alert_reason.is_none());

        // Healthy, and quiet-by-design, both render no banner.
        assert!(
            audit_row(&record(Some(AuditVerdict::Passed), Some(1_800_000_000)))
                .alert_reason
                .is_none()
        );
        assert!(
            audit_row(&record(
                Some(AuditVerdict::Unreachable {
                    error: "connect refused".into()
                }),
                Some(1_800_000_000),
            ))
            .alert_reason
            .is_none(),
            "Unreachable is quiet by design — the laptop-on-a-plane case"
        );

        // The three alerting states cross as their reason.
        let freshness = audit_row(&record(
            Some(AuditVerdict::FreshnessFailure { lag_secs: 300_000 }),
            Some(1_800_000_000),
        ));
        assert_eq!(
            freshness.alert_reason,
            Some(BackupAuditAlertReason::Freshness { lag_secs: 300_000 })
        );
        assert_eq!(freshness.last_passed_at, Some(1_800_000_000));
        assert_eq!(
            audit_row(&record(
                Some(AuditVerdict::InclusionFailure {
                    missing: 2,
                    sampled: 16
                }),
                None,
            ))
            .alert_reason,
            Some(BackupAuditAlertReason::Inclusion {
                missing: 2,
                sampled: 16
            })
        );
        assert_eq!(
            audit_row(&record(
                Some(AuditVerdict::Overdue {
                    since_secs: 900_000
                }),
                None,
            ))
            .alert_reason,
            Some(BackupAuditAlertReason::Overdue {
                since_secs: 900_000
            })
        );
    }

    /// A negative stamp is not a time this client could have written, so it
    /// narrows to "never" rather than wrapping into a huge `u64`.
    #[cfg(feature = "backup-audit")]
    #[test]
    fn audit_row_narrows_a_negative_stamp_to_never() {
        use fauna_client_backup::audit::{DestinationAuditRecord, DestinationAuditState};

        let row = audit_row(
            &DestinationAuditRecord {
                state: DestinationAuditState {
                    destination_id: "id-a".to_string(),
                    last_passed_at: Some(-1),
                    ..Default::default()
                },
                verdict: None,
            },
            1_800_000_000,
        );
        assert_eq!(row.last_passed_at, None);
    }

    /// An accepted source regression inside its recovery window crosses as the
    /// fifth reason in `alert_reasons`, beside a `Passed` verdict that leaves the
    /// single `alert_reason` empty (`backup-restore.md` § Background Tasks →
    /// accepted-regression bullet); once the window closes the list is empty.
    #[cfg(feature = "backup-audit")]
    #[test]
    fn audit_row_projects_an_open_recovery_window_as_a_reason() {
        use fauna_client_backup::audit::{
            AcceptedRegression, AuditVerdict, DestinationAuditRecord, DestinationAuditState,
        };
        use fauna_core::format::BackupAuditAlertReason;

        const NOW: i64 = 1_800_000_000;
        let mut state = DestinationAuditState {
            destination_id: "id-a".to_string(),
            last_passed_at: Some(NOW),
            ..Default::default()
        };
        state.accepted_regressions.insert(
            "ledger".to_string(),
            AcceptedRegression {
                pinned: 40,
                served: 30,
                observed_at: NOW,
                floored_at: None,
                recoverable_until: Some(NOW + 1_000),
            },
        );
        let record = DestinationAuditRecord {
            state,
            verdict: Some(AuditVerdict::Passed),
        };

        let open = audit_row(&record, NOW);
        assert!(open.alert_reason.is_none());
        assert_eq!(
            open.alert_reasons,
            vec![BackupAuditAlertReason::SourceRegressed {
                left_secs: Some(1_000)
            }]
        );
        assert!(audit_row(&record, NOW + 1_000).alert_reasons.is_empty());
    }

    /// The ms→s conversion belongs to this face, not to four shells
    /// (`backups.md` § Audit-alert surface pins the unit: a missing `/1000`
    /// "reads ~50 years ahead"). Also pins monotonicity end-to-end through the
    /// real file store, since an observation that could move backwards would
    /// quietly clear a genuine freshness failure.
    #[cfg(feature = "backup-audit")]
    #[test]
    fn observe_converts_ms_to_secs_and_only_ever_advances() {
        use fauna_client_backup::audit::AuditStateStore as _;

        let dir =
            std::env::temp_dir().join(format!("fauna-ffi-audit-observe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("audit-state.json");
        let _ = std::fs::remove_file(&path);
        let path_str = path.to_string_lossy().to_string();

        let read_high_water = || {
            fauna_client_backup::native_store::FileAuditStateStore::at(path.clone())
                .load()
                .unwrap_or_default()
                .observed_high_water
        };

        // Observations in the observer's past: the store clamps anything more
        // than `OBSERVATION_MAX_FUTURE_SKEW_SECS` ahead of its own clock, so a
        // fixed future date would read back as "now" rather than as itself.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after the epoch")
            .as_secs() as i64;
        let (older, observed, newer) = (now - 200 * 86_400, now - 100 * 86_400, now - 86_400);

        // Milliseconds in, seconds persisted — the unit assertion.
        assert!(backup_audit_observe(path_str.clone(), observed * 1000));
        assert_eq!(read_high_water(), Some(observed));

        // Older observation: no write, high-water unmoved.
        assert!(!backup_audit_observe(path_str.clone(), older * 1000));
        assert_eq!(read_high_water(), Some(observed));

        // Newer observation advances it.
        assert!(backup_audit_observe(path_str.clone(), newer * 1000));
        assert_eq!(read_high_water(), Some(newer));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn label_prefers_name_else_host() {
        assert_eq!(
            backup_destination_label(Some("Aunt's nest".into()), "https://a.example.com".into()),
            "Aunt's nest"
        );
        // Blank/absent name → the URL host (the shared fallback).
        assert_eq!(
            backup_destination_label(None, "https://b.example.com:8443".into()),
            "b.example.com"
        );
        assert_eq!(
            backup_destination_label(Some(String::new()), "https://b.example.com".into()),
            "b.example.com"
        );
    }
}
