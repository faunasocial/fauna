//! Re-exports the folder creation wizard machine so its UniFFI exports
//! surface in the generated Swift / Kotlin / C# bindings, plus a free-fn
//! constructor that builds the machine over an [`FfiNestClient`]'s WS-RPC
//! connection. The machine itself lives in libs/fauna-folders-machine; this
//! file is a thin glue layer (mirrors src/mail_admin.rs).

use std::sync::Arc;

pub use fauna_folders_machine::{
    ConflictPolicyOption, DeviceOption, DevicePlacesSnapshot, EnrolledDeviceSummary,
    FolderWizardError, FolderWizardMachine, FolderWizardObserver, FolderWizardSnapshot,
    FolderWizardStep, NameSnapshot, NestPlaceEdit, NestPlaceWrite, NestSnapshotsOption,
    RetentionPolicy, ReviewSnapshot, SubmitPhase, WizardDevice,
};

use crate::FfiNestClient;
use fauna_core::localized::LocalizedText;

/// Build a [`FolderWizardMachine`] for the Devices-page folder creation
/// wizard over `nest`'s authenticated WS-RPC connection. `available_devices` is
/// the Devices page's current enrollable-device list; `observer` ticks on every
/// snapshot change. `submit()` issues `fauna.folders.create` +
/// `fauna.folders.places.set` over the same connection.
#[uniffi::export]
pub fn build_folder_wizard_machine(
    nest: Arc<FfiNestClient>,
    observer: Arc<dyn FolderWizardObserver>,
    available_devices: Vec<DeviceOption>,
) -> Arc<FolderWizardMachine> {
    fauna_folders_machine::build_folder_wizard_machine(
        nest.nest_arc(),
        folder_custody(),
        observer,
        available_devices,
    )
}

/// The seat's folder-key custody for a folder create/delete helper — `None` on
/// a build with no account runtime, where the machines refuse both.
fn folder_custody() -> Option<Arc<dyn fauna_client_folders::FolderKeyStore>> {
    #[cfg(feature = "account-runtime")]
    {
        Some(crate::account_runtime::folder_key_store())
    }
    #[cfg(not(feature = "account-runtime"))]
    {
        None
    }
}

/// The canonical conflict-policy picker option list (wire value + i18n label
/// key) — the single source of the `folder-conflict-policy-select` /
/// `sync-default-conflict-policy-select` option *set* (folders.md § Where
/// logic lives). Replaces the
/// per-app hand-rolled `["auto", "latest_wins_always"]` + label-map copies.
/// It is `folders`-gated, so it's dropped from the Go
/// `--no-default-features` build (no `mail-bridge-ffi` restale).
#[uniffi::export]
pub fn conflict_policy_options() -> Vec<ConflictPolicyOption> {
    fauna_folders_machine::conflict_policy_options()
}

/// Resolve a *stored* conflict-policy wire value (`"auto"` /
/// `"latest_wins_always"`) to its picker label as a [`LocalizedText`]; an
/// unknown value degrades to `Auto`'s label, matching
/// `ConflictPolicy::from_wire`. Mirrors [`mode_label`].
#[uniffi::export]
pub fn conflict_policy_label(value: String) -> LocalizedText {
    fauna_folders_machine::conflict_policy_label(&value)
}

/// The canonical member-access picker option list (wire value + i18n label) —
/// the single source of the `folder-share-role-select` /
/// `folder-member-role-select` option *set* (multi-writer Phase 1,
/// `ui/folders.md` § Sharing). Mirrors [`conflict_policy_options`]; same
/// `folders` gating (dropped from the Go build).
#[uniffi::export]
pub fn member_access_options() -> Vec<fauna_folders_machine::MemberAccessOption> {
    fauna_folders_machine::member_access_options()
}

/// The canonical `folder-nest-snapshots-select` option list (wire value + i18n
/// label) — the three-state keeps-snapshots knob of the nest place's policy
/// (`docs/goal/behavior/backup-restore.md` § 8b). Mirrors
/// [`conflict_policy_options`]; the default state leads, because it is where the
/// knob rests and where it returns when its owner picks the default.
#[uniffi::export]
pub fn nest_snapshots_options() -> Vec<fauna_folders_machine::NestSnapshotsOption> {
    fauna_folders_machine::nest_snapshots_options()
}

/// Resolve a `folder-nest-snapshots-select` value to its label; an unrecognized
/// value degrades to the default state's label, so an unknown value is never
/// read back as an explicit on/off the owner never chose.
#[uniffi::export]
pub fn nest_snapshots_label(value: String) -> LocalizedText {
    fauna_folders_machine::nest_snapshots_label(&value)
}

/// Seed the four `folder-nest-*` controls from a folder row's `nest_snapshots` /
/// `nest_snapshot_quiet_secs` / `retention_policy`.
///
/// An unset knob prefills **blank** — and so does a **zero** retention bound,
/// because zero is the nest's own spelling of unset
/// (`backup/retention.rs::parse_folder_retention`). An app that renders the `0`
/// turns an unset bound into one the owner appears to have chosen, and the two
/// spellings then drift on the next save.
#[uniffi::export]
pub fn nest_place_edit_from_row(
    nest_snapshots: Option<bool>,
    nest_snapshot_quiet_secs: Option<i64>,
    retention_policy: Option<String>,
) -> fauna_folders_machine::NestPlaceEdit {
    fauna_folders_machine::nest_place_edit_from_row(
        nest_snapshots,
        nest_snapshot_quiet_secs,
        retention_policy,
    )
}

/// Read the four `folder-nest-*` controls into the whole policy
/// `DevicesMachine::set_folder_nest_place` takes — the inverse of
/// [`nest_place_edit_from_row`], and the only sanctioned way to build that call.
///
/// Every knob rides on every save, because the nest applies the policy whole. ⚠
/// **Retention inverts the rule**: its wire `None` means *leave unchanged*, so a
/// cleared retention comes back as the canonical binds-nothing policy, never
/// `None`. An app that hand-rolls this call and passes `None` for a retention
/// the user just emptied leaves the old bounds in force, silently.
#[uniffi::export]
pub fn nest_place_write(
    edit: fauna_folders_machine::NestPlaceEdit,
) -> fauna_folders_machine::NestPlaceWrite {
    fauna_folders_machine::nest_place_write(&edit)
}

/// Seed the two version-retention boxes (`folder-version-retention-count` /
/// `-days`, the § 8b FOURTH per-place knob — `file-versions.md` § Retention)
/// from a `FolderSummary::version_retention`'s bounds, flattened to its two
/// numbers (absent policy = `(0, 0)`). A zero bound prefills **blank** — the
/// same unset-never-renders-as-`0` rule as [`nest_place_edit_from_row`].
#[uniffi::export]
pub fn version_retention_edit_from_bounds(
    max_versions_per_path: u32,
    max_age_days: u32,
) -> fauna_folders_machine::VersionRetentionEdit {
    fauna_folders_machine::version_retention_edit_from_bounds(max_versions_per_path, max_age_days)
}

/// Read the two version-retention boxes into the whole policy
/// `DevicesMachine::set_folder_nest_place`'s `version_retention` arg takes.
/// Blank boxes ⇒ the binds-nothing policy, which CLEARS (the nest rests
/// `NULL`); an app whose editor lacks the knobs passes `null`/`nil` for the
/// whole arg instead — *leave unchanged* — and must never call this just to
/// fill the slot.
#[uniffi::export]
pub fn version_retention_write(
    edit: fauna_folders_machine::VersionRetentionEdit,
) -> fauna_folders_machine::VersionRetentionWrite {
    fauna_folders_machine::version_retention_write(&edit)
}

/// Resolve a stored member-access wire value (`"reader"` / `"writer"`) to its
/// picker label; unknown/absent degrades to `reader`'s label (the fail-safe
/// absent-row default). Mirrors [`conflict_policy_label`].
#[uniffi::export]
pub fn member_access_label(value: String) -> LocalizedText {
    fauna_folders_machine::member_access_label(&value)
}

/// The localized `conflict-type-badge` text for one auto-resolve review row —
/// the resolution (`merged` / `latest-kept`) when resolved, else the conflict
/// type (`folders.md:104`). Derived from the three `ConflictSummary` fields
/// clients already hold (`resolution`, `resolved_at`, `conflict_type`), so it's
/// a pure client-side computation. Replaces five diverged per-app copies
/// (android leaked the raw type on resolved rows; the natives leaked the raw
/// type on the two known conflict types web localized). `folders`-gated, so
/// it's dropped from the Go `--no-default-features` build (no `mail-bridge-ffi`
/// restale).
#[uniffi::export]
pub fn conflict_badge_label(
    resolution: Option<String>,
    resolved_at: Option<i64>,
    conflict_type: String,
) -> LocalizedText {
    fauna_folders_machine::conflict_badge_label(resolution.as_deref(), resolved_at, &conflict_type)
}

/// What one folder row's device-local binding section shows: whether the
/// `folder-location-*` rows appear at all, and whether
/// `folder-access-revoked-warning` heads them (`file-sync.md` § Multi-writer
/// shared sets → *Revocation*). `role` / `access` are the row's
/// `FolderSummary` fields; `parked` is whether any of this device's bindings
/// to the set is parked (the agent's `access_revoked` flag). Keying the row on
/// the access alone hides a demoted writer's parked binding — and its warning —
/// on the first refresh after the demotion, so every app asks this one
/// decision instead. `folders`-gated, like its neighbours.
#[uniffi::export]
pub fn binding_section(
    role: Option<String>,
    access: Option<String>,
    parked: bool,
) -> fauna_folders_machine::BindingSection {
    fauna_folders_machine::binding_section(role.as_deref(), access.as_deref(), parked)
}

/// Parse the `folder-include-paths` / `folder-exclude-paths` edit field into
/// the typed list `fauna.folders.update` takes (comma-split, trimmed,
/// empties dropped). Always returns a list — an emptied field parses to `[]`
/// (clear the filter), never an absent field (leave unchanged): the arm the
/// per-app copies this replaces had diverged on. `folders`-gated, so it's
/// dropped from the Go `--no-default-features` build (no `mail-bridge-ffi`
/// restale).
#[uniffi::export]
pub fn parse_paths_field(text: String) -> Vec<String> {
    fauna_folders_machine::parse_paths_field(&text)
}

/// Render stored selective-sync paths back into the single-line edit field
/// [`parse_paths_field`] reads: comma+space-joined; an absent or empty list
/// renders as `""`.
#[uniffi::export]
pub fn join_paths_field(paths: Option<Vec<String>>) -> String {
    fauna_folders_machine::join_paths_field(paths.as_deref())
}

/// The canonical `folder-audience-select` option list for one row — wire value +
/// i18n label + whether picking it is a real transition (`ui/folders.md`
/// § Audience and website serving).
///
/// Takes `bound` and the NORMALIZED `current` audience because **the option set
/// is exactly what the nest accepts from here**: `folder_handlers` validates
/// to-`public` from any audience, to-`private` only while unbound, to-`shared`
/// only while bound. So a bound row is offered `shared` + `public`, an unbound
/// row `private` + `public` — and `shared` is selectable exactly while the
/// bound folder is `public` (the flip-back, the one exit from its
/// public window; the pick re-seals the corpus for its members). Offering what
/// the nest would refuse produces a control that fails on click and a message
/// re-explaining a rule the picker could have honoured. Mirrors
/// [`member_access_options`]; same `folders` gating.
#[uniffi::export]
pub fn audience_options(
    bound: bool,
    current: String,
) -> Vec<fauna_folders_machine::AudienceOption> {
    fauna_folders_machine::audience_options(bound, &current)
}

/// The hint beside `folder-audience-select`, on the same `(bound, current)`
/// inputs as [`audience_options`] so the two cannot disagree about what the
/// picker offers: unbound explains private/public, bound points at the sharing
/// section, bound-and-`public` explains that picking Shared is the way back.
#[uniffi::export]
pub fn audience_hint(bound: bool, current: String) -> LocalizedText {
    fauna_folders_machine::audience_hint(bound, &current)
}

/// The audience a row should be RENDERED as, given the stored wire value and
/// whether the folder is group-bound — the value `folder-audience-select` paints.
///
/// A select showing a value outside its own option set is unpaintable, and the
/// case is reachable: an unrecognized or absent audience value reaches the
/// client (`FolderSummary` defaults it to `""`). **Fail-closed in the one direction that
/// matters** — anything unrecognized resolves to `shared` when bound and
/// `private` when not, *never* `public`. A binary that cannot parse the column
/// must not tell the user their folder is world-readable.
#[uniffi::export]
pub fn normalize_audience(value: String, bound: bool) -> String {
    fauna_folders_machine::normalize_audience(&value, bound)
}

/// The hint beside `folder-website-toggle` — a TRI-state on the live serving
/// picture (`ui/folders.md` § Audience and website serving).
///
/// Publishing a site takes switches in TWO places (this page's audience +
/// website toggle, and the actor's own web-address opt-in, default OFF), and a
/// user who flipped only the folder half was told nothing while the nest served
/// its info page in their site's place. `audience` is the **normalized** value
/// ([`normalize_audience`]); `address_enabled` rides
/// `DevicesSnapshot::website_address_enabled`, read best-effort with the page —
/// so `None` (unwired adapter, failed read) is a real arm, and it
/// hedges. The degrade direction is deliberate: unknown never claims the site is
/// live.
#[uniffi::export]
pub fn website_serve_hint(
    audience: String,
    paywalled: bool,
    address_enabled: Option<bool>,
) -> LocalizedText {
    fauna_folders_machine::website_serve_hint(&audience, paywalled, address_enabled)
}

/// The `folder-writer-published-warning` copy for one member (`ui/folders.md`
/// § Sharing), or `None` when the grant reaches nobody outside the set.
///
/// A `writer` grant on a `public` or paywalled folder changes what people
/// OUTSIDE the set read — the reach test is deliberately the one
/// [`website_serve_hint`] applies, so the two cannot drift. `access` is the
/// member's wire value (an absent row means reader); `audience` is the
/// **normalized** value ([`normalize_audience`]). Advisory only: it never
/// blocks the grant, and it stacks with the uncapped-quota warning.
#[uniffi::export]
pub fn writer_grant_reach(
    access: String,
    audience: String,
    paywalled: bool,
) -> Option<LocalizedText> {
    fauna_folders_machine::writer_grant_reach(&access, &audience, paywalled)
}

/// The localized label for a stored audience wire value — the read-only
/// rendering of a row's audience, where no picker is drawn.
///
/// An unrecognized value degrades to `private`'s label, the fail-CLOSED reading
/// and the only safe one: a string this binary could not parse must never be
/// painted `Public`. Mirrors [`member_access_label`].
#[uniffi::export]
pub fn audience_label(value: String) -> LocalizedText {
    fauna_folders_machine::audience_label(&value)
}

/// The `folder-nest-residency-select` picker: Full (default) then
/// Metadata-only, in that order on every app (folders re-model phase 5;
/// `file-sync.md` § Content residency). Both options are always selectable —
/// the flip to metadata-only is confirm-gated in the app, not withheld.
#[uniffi::export]
pub fn residency_options() -> Vec<fauna_folders_machine::ResidencyOption> {
    fauna_folders_machine::residency_options()
}

/// The residency a row should be RENDERED as, given the stored wire value —
/// the value `folder-nest-residency-select` paints. **Fail-closed to Full**:
/// only the exact `metadata_only` value paints as metadata-only, since that
/// reading is the claim the destructive confirm is gated on.
#[uniffi::export]
pub fn normalize_residency(value: String) -> String {
    fauna_folders_machine::normalize_residency(&value)
}

/// The localized label for a stored residency wire value — the read-only
/// rendering. Mirrors [`audience_label`].
#[uniffi::export]
pub fn residency_label(value: String) -> LocalizedText {
    fauna_folders_machine::residency_label(&value)
}

/// The hint beside `folder-nest-residency-select`, on the same NORMALIZED
/// value the select paints so copy and control agree: a full folder explains
/// what the nest's copy buys, a metadata-only folder states the availability
/// cost it accepted.
#[uniffi::export]
pub fn residency_hint(current: String) -> LocalizedText {
    fauna_folders_machine::residency_hint(&current)
}

/// The four phase-4 audience/website decisions, pinned **through the boundary
/// face** rather than in the shared crate alone.
///
/// The shared functions have their own tests; what these add is that the face
/// EXISTS and carries the decision unaltered. That is the defect class this
/// batch was written against: `ui/folders.md` § Implementation status today
/// records that `fauna-wasm-folders` carried none of phase 2 slice e's gestures
/// even though the shared Rust was complete, so an app leg could not reach a
/// rule that was, in every other sense, already built. A face that silently
/// re-derives or drops a rule fails the same way.
#[cfg(test)]
mod audience_face_tests {
    /// The option set is exactly what the nest accepts from here
    /// (`ui/folders.md` § Audience and website serving, the transition table).
    #[test]
    fn the_audience_options_offer_only_reachable_transitions() {
        let unbound = super::audience_options(false, "private".to_string());
        assert_eq!(
            unbound.iter().map(|o| o.value.as_str()).collect::<Vec<_>>(),
            ["private", "public"],
            "an unbound folder's two real transitions"
        );
        assert!(
            unbound.iter().all(|o| o.selectable),
            "both are reachable from unbound, so both are selectable"
        );

        let bound = super::audience_options(true, "shared".to_string());
        assert_eq!(
            bound.iter().map(|o| o.value.as_str()).collect::<Vec<_>>(),
            ["shared", "public"],
            "a bound folder renders `shared` and is offered `public`; `private` is \
             withheld because the nest refuses it while bound"
        );
        assert!(
            !bound[0].selectable,
            "`shared` is a state to render while the folder is not public — \
             bound-ness is entered through the share flow and nowhere else"
        );
        assert!(
            bound[1].selectable,
            "`public` is a real transition from bound"
        );

        // once the bound folder IS public, `shared` is the one exit —
        // the flip-back that re-seals the corpus for its members.
        let bound_public = super::audience_options(true, "public".to_string());
        assert!(
            bound_public[0].selectable,
            "`shared` is the flip-back exit while a bound folder is public"
        );

        // The hint rides the same inputs, so the copy and the option set agree.
        assert_eq!(
            super::audience_hint(true, "public".to_string()).key,
            "devices.folder_audience_public_bound_hint"
        );
        assert_eq!(
            super::audience_hint(true, "shared".to_string()).key,
            "devices.folder_audience_shared_hint"
        );
        assert_eq!(
            super::audience_hint(false, "private".to_string()).key,
            "devices.folder_audience_hint"
        );
    }

    /// Fail-closed in the one direction that matters: an unparseable value must
    /// never resolve to `public` (`ui/folders.md` § Audience and website serving).
    #[test]
    fn normalize_never_resolves_an_unknown_value_to_public() {
        for value in ["", "wat", "PUBLIC", "publi"] {
            assert_eq!(
                super::normalize_audience(value.to_string(), false),
                "private",
                "unbound floor"
            );
            assert_eq!(
                super::normalize_audience(value.to_string(), true),
                "shared",
                "bound floor"
            );
        }
        assert_eq!(
            super::normalize_audience("public".to_string(), false),
            "public"
        );
        assert_eq!(
            super::normalize_audience("public".to_string(), true),
            "public"
        );
        assert_eq!(
            super::normalize_audience("shared".to_string(), false),
            "private",
            "`shared` is only coherent for a bound folder"
        );
    }

    /// Three states, and the unknown arm degrades AWAY from claiming the site is
    /// live (`ui/folders.md` § Audience and website serving).
    #[test]
    fn the_website_hint_is_a_tri_state_that_degrades_away_from_live() {
        let inert = super::website_serve_hint("private".to_string(), false, None);
        assert_eq!(inert.key, "devices.serve_website_needs_audience");

        let live = super::website_serve_hint("public".to_string(), false, Some(true));
        assert_eq!(live.key, "devices.serve_website_live");

        let address_off = super::website_serve_hint("public".to_string(), false, Some(false));
        assert_eq!(address_off.key, "devices.serve_website_address_off");

        let unknown = super::website_serve_hint("public".to_string(), false, None);
        assert_eq!(
            unknown.key, "devices.serve_website_hint",
            "an unreadable address flag hedges — it must never claim the site is live"
        );
        assert_ne!(unknown.key, live.key);

        assert_eq!(
            super::website_serve_hint("private".to_string(), true, Some(true)).key,
            "devices.serve_website_live",
            "a paywalled folder has a readable audience even when not `public`"
        );
    }

    /// The published-folder writer warning is a decision, not a copy string: the
    /// export must hand each app the ONE key (or nothing) so no app re-derives the
    /// reach test (`ui/folders.md` § Sharing).
    #[test]
    fn the_writer_reach_warning_picks_one_sentence_or_none() {
        assert_eq!(
            super::writer_grant_reach("writer".to_string(), "public".to_string(), false)
                .map(|t| t.key),
            Some("devices.writer_public_warning".to_string())
        );
        assert_eq!(
            super::writer_grant_reach("writer".to_string(), "shared".to_string(), true)
                .map(|t| t.key),
            Some("devices.writer_paywalled_warning".to_string())
        );
        assert!(
            super::writer_grant_reach("writer".to_string(), "shared".to_string(), false).is_none(),
            "a writer on a folder that reaches nobody outside its members has nothing to warn about"
        );
        assert!(
            super::writer_grant_reach("reader".to_string(), "public".to_string(), true).is_none(),
            "a reader grant changes nothing anyone reads"
        );
    }

    /// An unrecognized value degrades to `private`'s label — never `Public`, on
    /// the strength of a string this binary could not parse.
    #[test]
    fn the_audience_label_degrades_closed() {
        assert_eq!(
            super::audience_label("public".to_string()).key,
            "devices.folder_audience_public"
        );
        assert_eq!(
            super::audience_label("shared".to_string()).key,
            "devices.folder_audience_shared"
        );
        for value in ["", "wat", "private"] {
            assert_eq!(
                super::audience_label(value.to_string()).key,
                "devices.folder_audience_private",
                "unknown degrades to the fail-closed label"
            );
        }
    }
}
