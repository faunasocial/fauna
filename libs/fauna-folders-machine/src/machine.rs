//! The folder creation wizard state machine.
//!
//! Mirrors `fauna_onboarding_machine::OnboardingMachine`: each app holds an
//! `Arc<FolderWizardMachine>`, observes via a registered
//! `FolderWizardObserver`, mutates via the gesture methods, and renders off
//! the per-step snapshots (or the aggregate `snapshot()`). State is in-memory
//! only — opening / closing the wizard is the Devices page's job (construct on
//! open; drop on cancel; `step() == Done` after a successful `submit()` tells
//! the client to close and refresh).
//!
//! Uses `std::sync::Mutex` (not tokio's) so getters and sync mutations work
//! from any thread context — including UI threads and `#[tokio::test]`. The
//! async `submit()` snapshots state under the lock, drops it, does IO, then
//! re-acquires; the lock is never held across an `await`.

use std::sync::{Arc, Mutex};

use fauna_core::localized::LocalizedText;

use crate::nest_api::{CreateFolderRequest, FolderNestApi, SetPlaceRequest};
use crate::observer::FolderWizardObserver;
use crate::snapshots::{
    DevicePlacesSnapshot, EnrolledDeviceSummary, FolderWizardSnapshot, NameSnapshot, ReviewSnapshot,
};
use crate::state::{
    DeviceOption, FolderWizardStep, RetentionPolicy, State, SubmitPhase, WizardDevice,
};
use fauna_protocol::folders::PlaceFlags;

/// i18n key for a `fauna.folders.create` failure ("Failed to create file
/// set: {message}").
const CREATE_ERROR_KEY: &str = "devices.wizard.create_error";
/// i18n key for a partial member-enroll failure, where the folder itself was
/// created but one or more `fauna.folders.places.set` calls failed ("File
/// set created, but some devices could not be enrolled: {message}"). Distinct
/// from `CREATE_ERROR_KEY` because the create succeeded; the structured
/// `ReviewSnapshot::{created, failed_members}` fields disambiguate further.
const MEMBER_ERROR_KEY: &str = "devices.wizard.create_member_error";

#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct FolderWizardMachine {
    state: Mutex<State>,
    observer: Arc<dyn FolderWizardObserver>,
    nest_api: Arc<dyn FolderNestApi>,
}

impl FolderWizardMachine {
    /// Open a fresh wizard over an injected [`FolderNestApi`] seam.
    /// `available_devices` is the device list the Devices page already has; each
    /// becomes an unselected `WizardDevice` at the default point.
    ///
    /// This is not a `#[uniffi::constructor]` — the seam (an
    /// `Arc<dyn FolderNestApi>`, e.g. the WS-RPC `WsRpcFolderNest`) has no FFI
    /// ABI. Clients construct via `nest_api::build_folder_wizard_machine`
    /// (native `fauna-ffi` / linux, wasm `fauna-wasm-folders`), which binds the
    /// session's connected requester; tests pass a `FakeFolderNestApi`.
    pub fn new(
        observer: Arc<dyn FolderWizardObserver>,
        available_devices: Vec<DeviceOption>,
        nest_api: Arc<dyn FolderNestApi>,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::new(to_wizard_devices(available_devices))),
            observer,
            nest_api,
        })
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl FolderWizardMachine {
    // ── Read surface ────────────────────────────────────────────────────

    pub fn step(&self) -> FolderWizardStep {
        self.with_state(|s| s.step)
    }

    pub fn name_snapshot(&self) -> NameSnapshot {
        self.with_state(build_name)
    }

    /// Step 2 as the place-flag checkboxes render it (phase 2 slice e).
    pub fn device_places_snapshot(&self) -> DevicePlacesSnapshot {
        self.with_state(build_device_places)
    }

    pub fn review_snapshot(&self) -> ReviewSnapshot {
        self.with_state(build_review)
    }

    /// The whole renderable wizard in one record (the form that slots into
    /// `DevicesSnapshot.wizard`).
    pub fn snapshot(&self) -> FolderWizardSnapshot {
        self.with_state(|s| FolderWizardSnapshot {
            step: s.step,
            name: build_name(s),
            device_places: build_device_places(s),
            review: build_review(s),
        })
    }

    // ── Gestures ────────────────────────────────────────────────────────

    pub fn set_name(&self, name: String) {
        self.mutate(|s| s.name = name);
    }

    /// Toggle whether device `index` (into the device list) is enrolled.
    /// Out-of-range indices are ignored.
    pub fn toggle_device_member(&self, index: u32) {
        self.mutate(|s| {
            if let Some(d) = s.devices.get_mut(index as usize) {
                d.selected = !d.selected;
            }
        });
    }

    /// Set device `index`'s place flags — the three
    /// `wizard-device-{originates,accepts,applies-deletes}` checkboxes.
    /// Out-of-range indices are ignored. Every one of the eight points is a
    /// valid place — there is nothing to refuse here.
    pub fn set_device_flags(
        &self,
        index: u32,
        originates: bool,
        accepts: bool,
        applies_deletes: bool,
    ) {
        self.mutate(|s| {
            if let Some(d) = s.devices.get_mut(index as usize) {
                d.originates = originates;
                d.accepts = accepts;
                d.applies_deletes = applies_deletes;
            }
        });
    }

    /// Inject the user's global default conflict policy (`"auto"` |
    /// `"latest_wins_always"` — the `fauna.state.sync-prefs` default conflict policy)
    /// so `submit()` stamps it onto the create request. Called by the client
    /// glue right after opening the wizard; `None` (the default) leaves new
    /// sets on the nest column default (`auto`). Not a user-visible wizard
    /// step — the per-set `folder-conflict-policy-select` is the post-create
    /// edit surface.
    pub fn set_default_conflict_policy(&self, policy: Option<String>) {
        self.mutate(|s| s.default_conflict_policy = policy);
    }

    /// Advance one step. No-op past `Review` (the create gesture is `submit()`)
    /// and blocked on `Name` until the name is non-empty.
    pub fn next(&self) {
        self.mutate(|s| {
            s.step = match s.step {
                FolderWizardStep::Name if s.name_valid() => FolderWizardStep::Devices,
                FolderWizardStep::Devices => FolderWizardStep::Review,
                other => other,
            };
        });
    }

    /// Step back one. No-op on the first step / terminal.
    pub fn back(&self) {
        self.mutate(|s| {
            s.step = match s.step {
                FolderWizardStep::Devices => FolderWizardStep::Name,
                FolderWizardStep::Review => FolderWizardStep::Devices,
                other => other,
            };
        });
    }

    /// Commit the folder: `fauna.folders.create`, then
    /// `fauna.folders.places.set` for each enrolled device. On full success
    /// advances to `Done`. On create failure stays on `Review` with
    /// `submit_phase == Failed`. On partial failure (create ok, some member adds
    /// failed) stays on `Review` with the folder already created and the failed
    /// devices recorded; calling `submit()` again retries only the failed
    /// members.
    ///
    /// No-op (returns the current step) unless invoked on `Review` with a valid
    /// name.
    pub async fn submit(&self) -> FolderWizardStep {
        // Phase 1: validate + snapshot inputs under the lock.
        let (name, retention, conflict_policy, already_created, selected) = {
            let mut s = self.state.lock().unwrap();
            if s.step != FolderWizardStep::Review || !s.name_valid() {
                return s.step;
            }
            s.submit_phase = SubmitPhase::Submitting;
            s.error = None;
            let selected: Vec<(String, PlaceFlags)> = s
                .devices
                .iter()
                .filter(|d| d.selected)
                .map(|d| (d.device_id.clone(), d.place_flags()))
                .collect();
            (
                s.name.clone(),
                s.retention,
                s.default_conflict_policy.clone(),
                s.created,
                selected,
            )
        };
        self.observer.on_changed();

        // Phase 2: create (skip if a prior partial-failure submit already did).
        if !already_created {
            let req = CreateFolderRequest {
                name: name.clone(),
                // `None` for every plain create — the nest place's policy rests
                // unset until the owner edits it; a preset stamps its own
                // (`ui/folders.md` § Photo backup).
                retention_policy: retention,
                conflict_policy,
            };
            match self.nest_api.create_folder(req).await {
                Ok(()) => {
                    let mut s = self.state.lock().unwrap();
                    s.created = true;
                    s.pending_member_ids = selected.iter().map(|(id, _)| id.clone()).collect();
                }
                Err(e) => {
                    // Producer-side log for the reactive `error-message` banner:
                    // fire once here where the failure is recorded, not in the
                    // per-tick render (observability.md § Log on the *event*,
                    // not the *paint*). `log_line` is redaction-safe.
                    let err = submit_error(CREATE_ERROR_KEY, e.detail());
                    tracing::warn!(target: "fauna_folders", "{}", err.log_line());
                    self.mutate(|s| {
                        s.submit_phase = SubmitPhase::Failed;
                        s.error = Some(err);
                    });
                    return FolderWizardStep::Review;
                }
            }
        }

        // Phase 3: enroll the still-pending members.
        let pending = self.with_state(|s| s.pending_member_ids.clone());
        let mut still_pending: Vec<String> = Vec::new();
        let mut last_detail: Option<String> = None;
        for device_id in pending {
            // The fallback is the DEFAULT point, not `PlaceFlags::default()`:
            // `default()` is all-false (a seat that does nothing), and this arm
            // serves a device still pending from an earlier partial submit that
            // is no longer selected — it keeps what every enrolment has always
            // been.
            let flags = selected
                .iter()
                .find(|(id, _)| *id == device_id)
                .map(|(_, f)| f.clone())
                .unwrap_or(PlaceFlags::default_place());
            let place = SetPlaceRequest {
                device_id: device_id.clone(),
                flags,
            };
            if let Err(e) = self.nest_api.set_place(&name, place).await {
                still_pending.push(device_id);
                last_detail.get_or_insert_with(|| e.detail().to_string());
            }
        }

        // Phase 4: settle.
        if still_pending.is_empty() {
            self.mutate(|s| {
                s.pending_member_ids.clear();
                s.submit_phase = SubmitPhase::Done;
                s.error = None;
                s.step = FolderWizardStep::Done;
            });
            FolderWizardStep::Done
        } else {
            // Producer-side log for the reactive `error-message` banner (same
            // rule as the create-phase failure above). observability.md § Log
            // on the *event*, not the *paint*.
            let err = submit_error(MEMBER_ERROR_KEY, &last_detail.unwrap_or_default());
            tracing::warn!(target: "fauna_folders", "{}", err.log_line());
            self.mutate(|s| {
                s.pending_member_ids = still_pending;
                s.submit_phase = SubmitPhase::Failed;
                s.error = Some(err);
            });
            FolderWizardStep::Review
        }
    }
}

// ── Internal helpers (not FFI-exported: closures / Arc<dyn> have no ABI) ──
impl FolderWizardMachine {
    /// Stamp a retention policy onto the create — a PRESET's gesture, never a
    /// wizard step: a plain create stamps none (`ui/folders.md` § Photo
    /// backup). Crate-internal because the only caller is
    /// [`crate::photo_library::ensure_photo_library_set`].
    pub(crate) fn set_retention(&self, retention: Option<RetentionPolicy>) {
        self.mutate(|s| s.retention = retention);
    }

    fn with_state<R>(&self, f: impl FnOnce(&State) -> R) -> R {
        let guard = self.state.lock().unwrap();
        f(&guard)
    }

    fn mutate<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let result = {
            let mut guard = self.state.lock().unwrap();
            f(&mut guard)
        };
        self.observer.on_changed();
        result
    }
}

fn to_wizard_devices(devices: Vec<DeviceOption>) -> Vec<WizardDevice> {
    let default = PlaceFlags::default_place();
    devices
        .into_iter()
        .map(|d| WizardDevice {
            device_id: d.device_id,
            label: d.label,
            selected: false,
            originates: default.originates,
            accepts: default.accepts,
            applies_deletes: default.applies_deletes,
        })
        .collect()
}

fn submit_error(key: &str, detail: &str) -> LocalizedText {
    LocalizedText::key_arg(key, "message", detail.to_string())
}

pub(crate) fn build_name(s: &State) -> NameSnapshot {
    NameSnapshot {
        name: s.name.clone(),
        continue_enabled: s.name_valid(),
    }
}

fn build_device_places(s: &State) -> DevicePlacesSnapshot {
    DevicePlacesSnapshot {
        devices: s.devices.clone(),
        // Every flag point is a valid place, so no seat blocks this step. The
        // field stays because the step's shape is shared with every other
        // wizard step.
        continue_enabled: true,
    }
}

fn build_review(s: &State) -> ReviewSnapshot {
    let enrolled = s
        .devices
        .iter()
        .filter(|d| d.selected)
        .map(|d| EnrolledDeviceSummary {
            device_id: d.device_id.clone(),
            label: d.label.clone(),
        })
        .collect();
    // Failed-member labels are the still-pending devices after a partial
    // failure (empty before submit and after full success).
    let failed_members = s
        .pending_member_ids
        .iter()
        .map(|id| {
            s.devices
                .iter()
                .find(|d| &d.device_id == id)
                .map(|d| d.label.clone())
                .unwrap_or_else(|| id.clone())
        })
        .collect();
    ReviewSnapshot {
        name: s.name.clone(),
        retention: s.retention,
        enrolled,
        create_enabled: s.name_valid()
            && !matches!(s.submit_phase, SubmitPhase::Submitting | SubmitPhase::Done),
        phase: s.submit_phase,
        created: s.created,
        failed_members,
        error: s.error.clone(),
    }
}
