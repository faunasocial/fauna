//! UniFFI façade for the Task-delegation Settings surface (slice 4) — the native
//! (windows/apple/android) twin of the wasm SPA's binding over the one shared
//! `fauna_client_delegation::TaskDelegationView` all seven apps render
//! (`docs/goal/behavior/participants.md` § Task delegation; ui.yaml page
//! `task-delegation`).
//!
//! This module is a pure **type-mirror + transport** layer. All the policy — what
//! the assignment picker may offer, what a pin may target, who currently runs a
//! kind — lives once in `fauna_core::delegation` and is composed by the shared
//! async `TaskDelegationView`. The FFI here mirrors the shared types across the
//! UniFFI boundary (the shared enums embed the serde-only
//! [`fauna_core::data::ParticipantRef`], which carries no UniFFI derive, so each
//! gets an `Ffi*` mirror), wraps the view in a `uniffi::Object`, and marshals
//! errors into [`FfiError`]. It re-implements no policy (priority #2). Both
//! methods go through the shared
//! `fauna_sync_engine::preference_surfaces::{load_task_delegation_rows,
//! set_task_assignment}` (the same calls tui and linux make): the pins ride
//! the account store of this process's runtime
//! (`crate::account_runtime::handle_source()`, waited for when a call arrives
//! before the assembly has landed), while the live leases stay a nest call
//! (`config-dissolution.md` § The `__config` dissolution schedule → *The
//! closure order*, steps (1) and (5)).
//!
//! `LocalizedText` is NOT mirrored — it is a `fauna_core` `uniffi::Record`
//! (fauna-ffi enables `fauna-core/uniffi`), so [`FfiTaskDelegationRow::name`]
//! crosses the boundary directly, exactly as `value_format` / `moderation_badge`
//! return it (priority #2 — one registration, no per-surface copy).

use std::collections::HashMap;
use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_delegation::TaskDelegationView;
use fauna_core::data::ParticipantRef;
use fauna_core::delegation::{HeavyTaskCapability, PinOption, RunnerStatus, TaskDelegationRow};
use fauna_core::localized::LocalizedText;
use fauna_sync_engine::preference_surfaces;

use crate::{FfiError, general_err};

// ── Type mirrors ──

/// FFI mirror of [`fauna_core::data::ParticipantRef`] — one participant (a client
/// device, or one of the user's nests). The serde-only core type carries no
/// UniFFI derive, so this mirror crosses the boundary in both directions: it
/// appears in output rows (via [`FfiRunnerStatus`] / [`FfiPinOption`]) AND in the
/// pin-write input ([`FfiPinOption::Other`]).
///
/// The nest's 32-byte Ed25519 `actor_pubkey` rides as a UniFFI-friendly
/// `Vec<u8>`, validated back to 32 bytes on the inbound path.
#[derive(uniffi::Enum, Clone)]
pub enum FfiParticipantRef {
    /// A client device, keyed by its hex-encoded 32-byte device id.
    Device { device_id: String },
    /// A nest, keyed by its 32-byte Ed25519 actor pubkey.
    Nest { actor_pubkey: Vec<u8> },
}

impl FfiParticipantRef {
    /// The mirror of a shared-Rust ref, or `None` for a participant kind a
    /// newer build wrote ([`ParticipantRef::Unknown`]) — which
    /// `delegation_rows` never puts in a row, so this is the backstop that
    /// keeps the case out of the apps rather than a path that runs.
    fn from_core(r: ParticipantRef) -> Option<Self> {
        match r {
            ParticipantRef::Device { device_id } => Some(Self::Device { device_id }),
            ParticipantRef::Nest { actor_pubkey } => Some(Self::Nest {
                actor_pubkey: actor_pubkey.to_vec(),
            }),
            ParticipantRef::Unknown(_) => None,
        }
    }
}

impl FfiParticipantRef {
    /// Convert to the shared-Rust ref, validating the nest pubkey length.
    fn into_core(self) -> Result<ParticipantRef, FfiError> {
        Ok(match self {
            Self::Device { device_id } => ParticipantRef::Device { device_id },
            Self::Nest { actor_pubkey } => {
                let actor_pubkey: [u8; 32] =
                    actor_pubkey
                        .as_slice()
                        .try_into()
                        .map_err(|_| FfiError::General {
                            msg: "actor_pubkey must be 32 bytes".into(),
                        })?;
                ParticipantRef::Nest { actor_pubkey }
            }
        })
    }
}

/// FFI mirror of [`fauna_core::delegation::RunnerStatus`] — who currently runs a
/// task kind, for display on the Task-delegation surface.
#[derive(uniffi::Enum, Clone)]
pub enum FfiRunnerStatus {
    /// This device holds a fresh lease.
    ThisDevice,
    /// Another participant holds a fresh lease; the client resolves `who` to a
    /// display name from its roster.
    Other { who: FfiParticipantRef },
    /// No fresh lease holder — the kind waits for an eligible participant.
    Waiting,
}

impl FfiRunnerStatus {
    /// `None` when the runner is a participant this build cannot name
    /// ([`FfiParticipantRef::from_core`]).
    fn from_core(r: RunnerStatus) -> Option<Self> {
        Some(match r {
            RunnerStatus::ThisDevice => Self::ThisDevice,
            RunnerStatus::Other { who } => Self::Other {
                who: FfiParticipantRef::from_core(who)?,
            },
            RunnerStatus::Waiting => Self::Waiting,
        })
    }
}

impl FfiRunnerStatus {
    /// Convert to the shared-Rust status, validating any embedded nest pubkey.
    /// Needed only by [`task_delegation_runner_label`], which takes a runner
    /// status back across the boundary to render its label.
    fn into_core(self) -> Result<RunnerStatus, FfiError> {
        Ok(match self {
            Self::ThisDevice => RunnerStatus::ThisDevice,
            Self::Other { who } => RunnerStatus::Other {
                who: who.into_core()?,
            },
            Self::Waiting => RunnerStatus::Waiting,
        })
    }
}

/// FFI mirror of [`fauna_core::delegation::PinOption`] — one selectable option in
/// a task kind's assignment picker. Crosses both ways: outbound in a row's
/// `assignment` / `pin_options`, inbound as the [`FfiTaskDelegationView::set_assignment`]
/// argument.
#[derive(uniffi::Enum, Clone)]
pub enum FfiPinOption {
    /// No pin — the automatic policy order picks the runner (always first).
    Automatic,
    /// Pin to this device (offered only when this client is a runner).
    ThisDevice,
    /// Pin to some other participant; the client resolves `who` to a name.
    Other { who: FfiParticipantRef },
}

impl FfiPinOption {
    /// `None` when the option pins a participant this build cannot name
    /// ([`FfiParticipantRef::from_core`]).
    fn from_core(p: PinOption) -> Option<Self> {
        Some(match p {
            PinOption::Automatic => Self::Automatic,
            PinOption::ThisDevice => Self::ThisDevice,
            PinOption::Other { who } => Self::Other {
                who: FfiParticipantRef::from_core(who)?,
            },
        })
    }
}

impl FfiPinOption {
    /// Convert to the shared-Rust option (validating any embedded nest pubkey).
    fn into_core(self) -> Result<PinOption, FfiError> {
        Ok(match self {
            Self::Automatic => PinOption::Automatic,
            Self::ThisDevice => PinOption::ThisDevice,
            Self::Other { who } => PinOption::Other {
                who: who.into_core()?,
            },
        })
    }
}

/// FFI mirror of [`fauna_core::delegation::HeavyTaskCapability`] — whether this
/// client may ever run heavy task kinds (i.e. is a legal self-pin target).
///
/// **The shared type became per-(client, kind) on 2026-08-03** (participants.md
/// § The assignment picker), and this enum was a **two-arm** mirror while every
/// FFI desktop ran both heavy kinds and every FFI phone ran neither. **The four
/// apps stopped agreeing on 2026-08-15**, exactly as the two-arm doc warned
/// they one day would: macOS deleted its in-app segment-backup upload driver at
/// the slice-5 flip ([`backup-restore.md`] § Background Tasks → *Flip status
/// (slice 5)*), so it is now the anticipated "desktop that builds but does not
/// back up". Per that warning the enum **grew a third arm** rather than
/// widening [`Self::Runner`] back into a blanket "runs everything" — the
/// encoding that made the picker lie in the first place.
///
/// The heavy kinds an FFI client can run — **one** as of 2026-08-16:
///
/// - ~~`backup-upload`~~ — **RETIRED as a client kind 2026-08-16.** It was the
///   lease loop the (since-removed) `backup-lease` feature drove
///   (`LeaseRuntime`, `with_lease_gate`), retiring per app as each in-app
///   driver was deleted:
///   linux 2026-07-29, android 2026-08-15 (it never declared the kind), macOS
///   2026-08-15, **windows 2026-08-16 — the last declarant**. With it went the
///   `Runner` arm (its last caller) and the kind's `client_runnable` flag, which
///   is now `false`; the source nest is the writer.
/// - `index` — the content-index builder, wired by the `conversations_session`
///   factory on the desktop targets only, per the ratified build-vs-query split
///   ([`crate::index_launch::CLIENT_BUILDS_INDEX`]; content-index.md § Where
///   queries run — *a desktop builds and syncs; phones query the synced copy*).
///
/// The `index` half is **derived** from that same constant rather than restated,
/// on every arm that can declare it: the builder and the picker read one source,
/// so an `index` self-pin can only be offered where the builder actually runs.
/// Deleting a driver must retract the app's *declaration* too, not just the code
/// — that is how the per-kind half retires a client from a kind without touching
/// the kind's own flag (participants.md § The assignment picker, the linux
/// precedent), and skipping it strands users on a self-pin the build can only
/// ever wait on.
#[derive(uniffi::Enum, Clone, Copy)]
pub enum FfiHeavyTaskCapability {
    /// A native desktop that runs the content-index builder but ships **no**
    /// segment-backup upload driver — the source nest is the backup writer
    /// (`message-segment-store.md` § Cross-location backup protocol). macOS
    /// 2026-08-15, **windows 2026-08-16**; the shape linux and tui already declare
    /// natively (`runner_for([KIND_INDEX])`).
    ///
    /// ⚠ There is deliberately **no `Runner` arm any more.** It meant "also drives
    /// the `backup-upload` lease loop", and as of the slice-5 flip completing on
    /// 2026-08-16 **no app ships an in-app upload driver** — linux (2026-07-29),
    /// android (2026-08-15), apple (2026-08-15) and windows (2026-08-16) all
    /// deleted theirs, and the source nest has been the writer since 2026-07-24
    /// (`backup-restore.md` § Background Tasks → *Flip status (slice 5)*). Keeping
    /// the arm would let a future client re-declare a kind that
    /// `LIVE_TASK_KINDS` now marks `client_runnable: false`, offering a self-pin
    /// nothing can honour — the stranding this enum's own docs warn about. If an
    /// app ever ships a driver again, re-add the arm *and* flip that flag back
    /// together; neither alone is correct.
    IndexOnly,
    /// A phone: renders the surface, runs no heavy task kind. It still *queries*
    /// the synced content index — querying is not a heavy task kind and needs no
    /// pin.
    ViewerOnly,
}

/// The kinds an FFI client declares, as a **pure function** of the two
/// independent facts a build has: whether it runs the content-index builder, and
/// whether it drives the `backup-upload` lease loop.
///
/// Split out from the `From` impl below so the policy is testable at *every*
/// combination on any host. Folded inline it would only ever be exercised at the
/// host target's own value — on a desktop that means a mutation dropping the
/// index condition entirely still reads as correct, since a desktop *should*
/// claim `index`, and the phone half of the rule would be pinned by nothing at
/// all. (Verified: with the condition inlined, exactly that mutation survived the
/// suite.) The same argument now covers the backup half, which is why it is a
/// parameter rather than a per-arm literal.
fn declared_kinds(builds_index: bool, drives_backup_upload: bool) -> Vec<&'static str> {
    let mut kinds = Vec::new();
    if drives_backup_upload {
        kinds.push(fauna_core::delegation::KIND_BACKUP_UPLOAD);
    }
    if builds_index {
        kinds.push(fauna_core::delegation::KIND_INDEX);
    }
    kinds
}

impl From<FfiHeavyTaskCapability> for HeavyTaskCapability {
    fn from(c: FfiHeavyTaskCapability) -> Self {
        // Derived from the builder's own constant, never restated — see the enum
        // docs. On a phone target it is `false`, so even a client that wrongly
        // passed a desktop arm could not offer an `index` self-pin its build
        // cannot honour.
        let builds_index = crate::index_launch::CLIENT_BUILDS_INDEX;
        match c {
            FfiHeavyTaskCapability::IndexOnly => {
                HeavyTaskCapability::runner_for(declared_kinds(builds_index, false))
            }
            FfiHeavyTaskCapability::ViewerOnly => HeavyTaskCapability::viewer_only(),
        }
    }
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    /// The picker's half of the builder/picker agreement, pinned at **both**
    /// values rather than at whichever one this host happens to compile to.
    ///
    /// A phone declaring `index` would offer a self-pin nothing on that device
    /// can ever honour — the exact shape of the shipped `backup-upload`
    /// stranding the 2026-08-03 picker session found on linux, which is what
    /// made the per-(client, kind) encoding necessary in the first place.
    #[test]
    fn only_a_build_that_indexes_declares_the_index_kind() {
        let building = declared_kinds(true, true);
        assert!(building.contains(&fauna_core::delegation::KIND_INDEX));
        assert!(building.contains(&fauna_core::delegation::KIND_BACKUP_UPLOAD));

        let querying_only = declared_kinds(false, true);
        assert!(
            !querying_only.contains(&fauna_core::delegation::KIND_INDEX),
            "a build that only queries the synced index must not offer an \
             `index` self-pin — the kind would wait forever"
        );
        assert!(
            querying_only.contains(&fauna_core::delegation::KIND_BACKUP_UPLOAD),
            "withholding `index` must not withhold the lease loop a runner does drive"
        );
    }

    /// The backup half of the same rule, pinned independently of the index half.
    ///
    /// A build whose in-app upload driver has been deleted must stop *declaring*
    /// `backup-upload`, or the picker offers a self-pin that can only ever wait
    /// — the identical stranding, in the identical direction, that linux hit
    /// when its driver went 2026-07-29 while its declaration stayed
    /// (participants.md § The assignment picker). Deleting driver code without
    /// retracting the declaration is the mistake this pins.
    #[test]
    fn only_a_build_that_uploads_declares_the_backup_upload_kind() {
        let driverless = declared_kinds(true, false);
        assert!(
            !driverless.contains(&fauna_core::delegation::KIND_BACKUP_UPLOAD),
            "a build with no in-app upload driver must not offer a \
             `backup-upload` self-pin — the nest is the writer, and the pin \
             would wait forever"
        );
        assert!(
            driverless.contains(&fauna_core::delegation::KIND_INDEX),
            "retracting `backup-upload` must not retract the index builder a \
             driverless desktop does still run"
        );

        // The fourth corner: neither fact true declares nothing at all, which is
        // `viewer_only()`'s set by another route.
        assert!(declared_kinds(false, false).is_empty());
    }

    /// The two surviving arms map onto exactly the sets above — the arm-to-fact
    /// wiring itself, which the two tests above cannot see (they test the
    /// function, not which arm passes which argument). A mutation swapping
    /// `IndexOnly`'s `false` for `true` reddens only here.
    ///
    /// The old `Runner` arm's assertion is gone with the arm (slice-5 flip
    /// completed 2026-08-16 — no app ships an in-app upload driver). What
    /// replaces it is the stronger, arm-independent claim below: **no** arm may
    /// declare `backup-upload`, which is now a nest-run kind.
    #[test]
    fn the_arms_declare_what_their_builds_actually_run() {
        let index_only: HeavyTaskCapability = FfiHeavyTaskCapability::IndexOnly.into();
        let viewer: HeavyTaskCapability = FfiHeavyTaskCapability::ViewerOnly.into();

        assert!(
            !index_only.runs(fauna_core::delegation::KIND_BACKUP_UPLOAD),
            "the driverless-desktop arm must never declare `backup-upload` — \
             this is the whole reason the arm exists"
        );
        assert!(!viewer.runs(fauna_core::delegation::KIND_BACKUP_UPLOAD));
        assert!(!viewer.runs(fauna_core::delegation::KIND_INDEX));

        // The desktop arm agrees with the builder constant on the index half, so
        // it cannot offer a pin its build cannot honour.
        assert_eq!(
            index_only.runs(fauna_core::delegation::KIND_INDEX),
            crate::index_launch::CLIENT_BUILDS_INDEX
        );
    }

    /// No FFI arm may declare `backup-upload` any more. This is the guard the
    /// retired `Runner` arm used to make impossible to state: the kind is
    /// nest-run as of the slice-5 flip (`LIVE_TASK_KINDS.client_runnable ==
    /// false`), so a client arm that declared it would offer a self-pin nothing
    /// can honour — the stranding `participants.md` § The assignment picker
    /// describes, which linux actually shipped for four days in 2026-07.
    ///
    /// Deliberately exhaustive over the enum rather than a list of arms: adding
    /// a new arm that declares the kind fails here without anyone remembering to
    /// extend the test.
    #[test]
    fn no_ffi_arm_declares_the_nest_run_backup_upload_kind() {
        for arm in [
            FfiHeavyTaskCapability::IndexOnly,
            FfiHeavyTaskCapability::ViewerOnly,
        ] {
            let cap: HeavyTaskCapability = arm.into();
            assert!(
                !cap.runs(fauna_core::delegation::KIND_BACKUP_UPLOAD),
                "`backup-upload` is nest-run since the slice-5 flip — no client \
                 arm may declare it, or the picker offers a self-pin that \
                 silently stops the user being backed up at all"
            );
        }
    }
}

/// FFI mirror of [`fauna_core::delegation::TaskDelegationRow`] — one row of the
/// Task-delegation surface (a heavy task kind, its localizable display name, the
/// current runner, and the user's assignment plus the options the picker offers).
///
/// `name` is a `fauna_core` [`LocalizedText`] (a `uniffi::Record`) crossing the
/// boundary directly — no mirror — which the client resolves through its i18n
/// pipeline.
#[derive(uniffi::Record, Clone)]
pub struct FfiTaskDelegationRow {
    /// Stable kind string (`"backup-upload"`) — the key `set_assignment` takes.
    pub task_kind: String,
    /// The kind's display name as an i18n key (no args).
    pub name: LocalizedText,
    /// Who currently runs it.
    pub runner: FfiRunnerStatus,
    /// The user's current assignment — always an element of `pin_options`.
    pub assignment: FfiPinOption,
    /// Every option the picker may offer, in display order (`Automatic` first).
    pub pin_options: Vec<FfiPinOption>,
}

impl FfiTaskDelegationRow {
    /// `None` for a row naming a participant this build cannot name — which
    /// `delegation_rows` already withholds (see [`FfiParticipantRef::from_core`]).
    fn from_core(row: TaskDelegationRow) -> Option<Self> {
        Some(Self {
            task_kind: row.task_kind,
            name: row.name,
            runner: FfiRunnerStatus::from_core(row.runner)?,
            assignment: FfiPinOption::from_core(row.assignment)?,
            pin_options: row
                .pin_options
                .into_iter()
                .map(FfiPinOption::from_core)
                .collect::<Option<_>>()?,
        })
    }
}

// ── Label rendering (priority #2 — one shared decision, `fauna_core::delegation`) ──

/// Label a runner status for display. `labels` is the caller's device roster
/// (`device_id` → display name) — inherently client-side state, never derived
/// here. The returned [`LocalizedText`] crosses the boundary as-is; the client
/// resolves it through its own i18n pipeline (mirrors [`FfiTaskDelegationRow::name`]).
///
/// # Errors
///
/// `FfiError::General` if `runner` embeds a nest pubkey that is not 32 bytes.
#[uniffi::export]
pub fn task_delegation_runner_label(
    runner: FfiRunnerStatus,
    labels: HashMap<String, String>,
) -> Result<LocalizedText, FfiError> {
    Ok(fauna_core::delegation::runner_label(
        &runner.into_core()?,
        &labels,
    ))
}

/// Label one assignment-picker option for display. See
/// [`task_delegation_runner_label`] for the shape (same roster input, same
/// `LocalizedText` output, same shared decision in `fauna_core::delegation`).
///
/// # Errors
///
/// `FfiError::General` if `option` embeds a nest pubkey that is not 32 bytes.
#[uniffi::export]
pub fn task_delegation_option_label(
    option: FfiPinOption,
    labels: HashMap<String, String>,
) -> Result<LocalizedText, FfiError> {
    Ok(fauna_core::delegation::option_label(
        &option.into_core()?,
        &labels,
    ))
}

// ── The machine ──

/// FFI handle over the shared [`TaskDelegationView`] for one actor on one device.
///
/// Built Rust-side via [`crate::FfiNestClient::task_delegation_view_for_device`] (which
/// takes the non-FFI `NestClient` / keypair / participant ref), then handed to
/// the Swift/Kotlin glue as `Arc<Self>`. Unlike `FfiBackupCoordinator`, the view
/// is `Send + Sync` (it holds only `Arc<NestClient>` + built-in state, no
/// `!Send` SQLite handle), so it is awaited directly on the ambient tokio runtime
/// — no dedicated worker thread.
#[derive(uniffi::Object)]
pub struct FfiTaskDelegationView {
    inner: TaskDelegationView<Arc<NestClient>>,
}

impl FfiTaskDelegationView {
    /// Rust-side constructor — wraps a fully-built shared view. Returns the
    /// `Arc<Self>` shape UniFFI expects for `Object`-derived types.
    pub(crate) fn new(inner: TaskDelegationView<Arc<NestClient>>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

#[fauna_uniffi_async::export]
impl FfiTaskDelegationView {
    /// Read the surface — the user's pins joined with the live per-kind leases,
    /// composed into one row per live task kind. UniFFI exposes this to Swift as
    /// `async throws` and to Kotlin as a `suspend fun`.
    ///
    /// # Errors
    ///
    /// `FfiError::General` carrying the shared view's error chain on a
    /// account-store read or `fauna.delegation.observe` failure.
    pub async fn load(&self) -> Result<Vec<FfiTaskDelegationRow>, FfiError> {
        let store = crate::account_runtime::handle_source();
        let rows = preference_surfaces::load_task_delegation_rows(&store, &self.inner)
            .await
            .map_err(preference_surfaces::delegation_failure)
            .map_err(general_err)?;
        Ok(rows
            .into_iter()
            .filter_map(FfiTaskDelegationRow::from_core)
            .collect())
    }

    /// Write the user's assignment for `task_kind` (the picker's `onchange`),
    /// routed through the plane's read-modify-write of the pins record, so a
    /// concurrent device's edit to another kind is kept rather than clobbered.
    ///
    /// # Errors
    ///
    /// - `FfiError::General` carrying `NotPinnable`'s message if this client is
    ///   asked to pin itself for a kind it ships no runner for — a
    ///   [`FfiHeavyTaskCapability::ViewerOnly`] client for any kind, or an
    ///   [`FfiHeavyTaskCapability::IndexOnly`] one for anything but `index`
    ///   (the kind would then wait forever); nothing is sent on the wire.
    /// - `FfiError::General` if an embedded nest pubkey is not 32 bytes.
    /// - `FfiError::General` carrying the account-store write error chain otherwise.
    pub async fn set_assignment(
        &self,
        task_kind: String,
        option: FfiPinOption,
    ) -> Result<(), FfiError> {
        let option = option.into_core()?;
        let store = crate::account_runtime::handle_source();
        preference_surfaces::set_task_assignment(&store, &self.inner, &task_kind, &option)
            .await
            .map_err(preference_surfaces::delegation_failure)
            .map_err(general_err)?;
        Ok(())
    }
}
