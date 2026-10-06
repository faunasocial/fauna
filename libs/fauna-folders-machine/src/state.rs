//! Value types + the internal wizard state.
//!
//! The step flow is `Name → Devices → Review → Done` (`ui/folders.md`
//! § Layout & flow). The scan-frequency step that used to sit between
//! devices and review retired with phase 5 of the folders re-model
//! (`file-sync.md` § Config, the phase-5 block): the reconcile cadence is a
//! hard-coded constant, not a per-folder choice, and the wizard's retention
//! inputs retired with slice e (the nest place's `folder-nest-retention-*` is
//! the surviving editor). A folder has no type (`folders.md` § Target
//! re-model): the mode picker retired with the mode contraction, and a plain
//! create stamps no retention — only the Photo Library preset stamps one,
//! explicitly (`ui/folders.md` § Photo backup).

use serde::{Deserialize, Serialize};

use fauna_core::format::ConflictPolicy;
use fauna_core::localized::LocalizedText;

/// Wizard steps. `Done` is a sentinel: a successful `submit()` lands here and
/// the client should close the wizard and refresh its folder list. Mirrors
/// `OnboardingStep`'s terminal `Done`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FolderWizardStep {
    /// Step 1 — folder name.
    #[default]
    Name,
    /// Step 2 — per-device enrollment checkbox + the place's three flags.
    Devices,
    /// Step 3 — read-only review; the create gesture lives here. (The
    /// scan-frequency step that used to precede it retired with phase 5.)
    Review,
    /// Terminal — `submit()` succeeded; client closes the wizard.
    Done,
}

/// Lifecycle of the async `submit()` call, surfaced on the review snapshot so
/// clients can gate the create button + show progress / errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum SubmitPhase {
    #[default]
    Idle,
    /// `submit()` is in flight (create + member adds).
    Submitting,
    /// Everything committed; the machine has advanced to `Done`.
    Done,
    /// `submit()` failed. See `ReviewSnapshot::{created, failed_members, error}`
    /// for whether the folder itself was created and which member adds failed.
    Failed,
}

/// Construction input — an available device the wizard can enroll. The Devices
/// page already has this list; the machine turns each into a `WizardDevice`
/// (initially unselected, at the default point). Distinct from `WizardDevice`
/// so the caller doesn't have to set the wizard-internal `selected` / flag
/// fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DeviceOption {
    pub device_id: String,
    pub label: String,
}

/// One enrollable device in the wizard's device step.
///
/// **The three flags are the place** — the device step is
/// `wizard-device-originates` / `-accepts` / `-applies-deletes` checkboxes, and
/// their meaning is [`fauna_protocol::folders::PlaceFlags`]'s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct WizardDevice {
    /// Hex-encoded device id (matches the nest's `AddMemberRequest.device_id`).
    pub device_id: String,
    pub label: String,
    pub selected: bool,
    /// Files added or edited on this device upload to the rest of the folder.
    pub originates: bool,
    /// Remote changes land on this device.
    pub accepts: bool,
    /// A peer's delete deletes here too. `false` is the archive seat.
    pub applies_deletes: bool,
}

impl WizardDevice {
    /// This seat's flag point — the three checkboxes read as the one authority
    /// type, so no caller has to re-assemble them (and none can assemble them
    /// in the wrong order).
    pub fn place_flags(&self) -> fauna_protocol::folders::PlaceFlags {
        fauna_protocol::folders::PlaceFlags::new(
            self.originates,
            self.accepts,
            self.applies_deletes,
        )
    }
}

/// Backup retention policy. Serializes to the nest's
/// `CreateFolderRequest.retention_policy` JSON shape
/// (`{"max_snapshots": N, "max_age_days": M}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RetentionPolicy {
    pub max_snapshots: u32,
    pub max_age_days: u32,
}

/// A canonical conflict-policy picker option: the machine owns the option
/// *set* (values + label keys); the label is an i18n key the client resolves. Clients build the
/// `folder-conflict-policy-select` and `sync-default-conflict-policy-select`
/// pickers from this list rather than hand-rolling the value list + label map
/// per platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ConflictPolicyOption {
    /// The canonical wire/DB value ([`ConflictPolicy::as_str`] — `"auto"` /
    /// `"latest_wins_always"`), what the select writes to
    /// `folders.conflict_policy` / `sync_prefs.default_conflict_policy` and
    /// what the cross-app `select(id, value)` e2e contract drives.
    pub value: String,
    pub label: LocalizedText,
}

/// The canonical conflict-policy picker order — `Auto` (the column default)
/// first, mirroring every shipped picker.
const CONFLICT_POLICY_ORDER: [ConflictPolicy; 2] =
    [ConflictPolicy::Auto, ConflictPolicy::LatestWinsAlways];

/// i18n label key for a conflict policy's picker option
/// (`devices.conflict_policy_*`).
fn conflict_policy_label_key(policy: ConflictPolicy) -> &'static str {
    match policy {
        ConflictPolicy::Auto => "devices.conflict_policy_auto",
        ConflictPolicy::LatestWinsAlways => "devices.conflict_policy_latest_wins",
    }
}

/// The canonical conflict-policy picker option list (wire value + i18n label
/// key). Derived from
/// `fauna_core::format::ConflictPolicy` so the value list cannot drift from
/// what the nest's `folders.conflict_policy` column accepts.
pub fn conflict_policy_options() -> Vec<ConflictPolicyOption> {
    CONFLICT_POLICY_ORDER
        .into_iter()
        .map(|policy| ConflictPolicyOption {
            value: policy.as_str().to_string(),
            label: LocalizedText::key(conflict_policy_label_key(policy)),
        })
        .collect()
}

/// The localized label for a stored conflict-policy wire value, looked up via
/// [`ConflictPolicy::from_wire`] — an unknown value degrades to `Auto`'s label,
/// matching the wire parse (both arms retain the loser, so degrading is safe)
/// and the `_ =>` arm of every per-app copy this replaces.
pub fn conflict_policy_label(value: &str) -> LocalizedText {
    LocalizedText::key(conflict_policy_label_key(ConflictPolicy::from_wire(value)))
}

/// One `folder-audience-select` option — a folder's audience
/// (`folders.md` § Target re-model owns the model). Mirrors
/// [`ConflictPolicyOption`]: every app builds its picker from this list, so the
/// seven cannot drift on which transitions they even offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AudienceOption {
    /// The canonical wire/DB value — `"private"` / `"shared"` / `"public"`, what
    /// `fauna.folders.update` accepts and what `folder_handlers::audience_of`
    /// derives on the way back.
    pub value: String,
    pub label: LocalizedText,
    /// Whether picking this option is a real transition **from the folder's
    /// current state**, or merely the rendering of that state.
    ///
    /// `false` for exactly one case: `shared` on a group-bound folder that is
    /// not currently `public`. Bound-ness is entered through the share flow and
    /// nowhere else, so `shared` is then a destination no audience control can
    /// reach — it is shown because a bound folder must be able to say what it
    /// is. Once the folder IS `public`, `shared` becomes the one legal exit
    /// (the flip-back): the nest accepts `→shared` exactly while
    /// bound, and picking it is what re-seals the corpus for its members.
    pub selectable: bool,
}

/// The audience picker for a folder, given whether it is group-bound and the
/// folder's current (NORMALIZED — [`normalize_audience`]) audience.
///
/// **The option set is exactly what the nest accepts from here**, which is why
/// this takes `bound` and `current` rather than returning a fixed list.
/// `folder_handlers` validates three transitions: to `public` from any
/// audience, to `private` only while unbound, to `shared` only while bound. So
/// a bound folder is offered `shared` and `public`, an unbound folder `private`
/// and `public`. Withholding `private` from a bound folder is not a UI
/// opinion — offering it would be a control that fails on click, and the
/// honest repair is to remove the sharing first.
///
/// `shared` on a bound folder is **selectable exactly while the folder is
/// `public`** — the flip-back, the one legal exit from a bound
/// folder's public window, which re-seals the corpus for its members
/// (`SyncEngine::converge_corpus_to_audience` on every seat, off the projected
/// audience alone). Anywhere else it is the rendering of the current state:
/// bound-ness is entered through the share flow, never through this picker.
///
/// `public` is offered from either state and always carries the owner confirm
/// (`folder-audience-public-confirm`): it rests the folder UNSEALED, names and
/// paths included (`principles.md` § The user always controls their data owns
/// that one exception).
pub fn audience_options(bound: bool, current: &str) -> Vec<AudienceOption> {
    let mut out = Vec::with_capacity(2);
    if bound {
        out.push(AudienceOption {
            value: AUDIENCE_SHARED.to_string(),
            label: LocalizedText::key("devices.folder_audience_shared"),
            selectable: current == AUDIENCE_PUBLIC,
        });
    } else {
        out.push(AudienceOption {
            value: AUDIENCE_PRIVATE.to_string(),
            label: LocalizedText::key("devices.folder_audience_private"),
            selectable: true,
        });
    }
    out.push(AudienceOption {
        value: AUDIENCE_PUBLIC.to_string(),
        label: LocalizedText::key("devices.folder_audience_public"),
        selectable: true,
    });
    out
}

/// The hint beside `folder-audience-select`, on the same (bound, current)
/// inputs as [`audience_options`] so the two can never disagree about what the
/// picker offers.
///
/// Three states: an unbound folder explains the private/public choice; a bound
/// folder explains that sharing is edited in the sharing section (its picker
/// offers no other exit); a bound folder currently `public` explains the one
/// exit its picker does offer — picking `shared` re-seals the folder for its
/// members, while anything published during the public window should be
/// treated as public for good (the same honesty the declassify confirm led
/// with).
pub fn audience_hint(bound: bool, current: &str) -> LocalizedText {
    LocalizedText::key(if bound && current == AUDIENCE_PUBLIC {
        "devices.folder_audience_public_bound_hint"
    } else if bound {
        "devices.folder_audience_shared_hint"
    } else {
        "devices.folder_audience_hint"
    })
}

/// The audience a folder should be RENDERED as, given the wire value and
/// whether it is group-bound.
///
/// Exists because `folder-audience-select`'s value must always be one of the
/// options [`audience_options`] offered — a select showing a value outside its
/// own option set is an unpaintable state, and the case is reachable: any
/// unparseable value (an empty string included) reaches it, and `FolderSummary`
/// defaults an absent value to the empty string.
///
/// **Fail-closed, in the only direction that matters.** Anything unrecognized
/// resolves to `shared` when bound and `private` when not — never `public`. A
/// binary that cannot parse the value must not tell the user their folder is
/// world-readable, nor invite them to publish into one it cannot vouch for.
pub fn normalize_audience(value: &str, bound: bool) -> String {
    match value {
        AUDIENCE_PUBLIC => AUDIENCE_PUBLIC.to_string(),
        // `shared` is only coherent for a bound folder; an unbound row claiming
        // it is as unparseable as an empty string, and falls to the same floor.
        AUDIENCE_SHARED if bound => AUDIENCE_SHARED.to_string(),
        _ if bound => AUDIENCE_SHARED.to_string(),
        _ => AUDIENCE_PRIVATE.to_string(),
    }
}

/// The hint painted beside `folder-website-toggle` — a TRI-state keyed on the
/// live serving picture (`ui/folders.md` § Audience and website serving).
///
/// Why three states and not the original two: publishing a site takes switches
/// in TWO places — the folder's audience + website toggle here, and the actor's
/// own web-address opt-in on the Web settings page (default OFF) — and a user
/// who flipped only the folder half was told nothing while the nest served its
/// own info page in their site's place (measured, phase 4 slice 4e). With the
/// address flag in hand the hint says the true thing:
///
/// * neither `public` nor paywalled → the audience wording (the toggle is real
///   but inert — nothing can serve to anyone yet);
/// * address known **ON** → the site is genuinely reachable: say so, plainly;
/// * address known **OFF** → the one misleading case, called out pointedly —
///   published here, reachable by nobody, and where to fix it;
/// * address **unknown** (`None` — an unwired adapter, a failed
///   read) → the combined wording that predates the flag, which hedges both
///   halves. The degrade direction is deliberate: unknown must never claim the
///   site is live.
///
/// `audience` is the **normalized** value ([`normalize_audience`]); the flag
/// rides the devices snapshot (`DevicesSnapshot::website_address_enabled`),
/// read best-effort with the page. One shared decision so no app re-derives it
/// (priority #2).
pub fn website_serve_hint(
    audience: &str,
    paywalled: bool,
    address_enabled: Option<bool>,
) -> LocalizedText {
    if audience != AUDIENCE_PUBLIC && !paywalled {
        return LocalizedText::key("devices.serve_website_needs_audience");
    }
    match address_enabled {
        Some(true) => LocalizedText::key("devices.serve_website_live"),
        Some(false) => LocalizedText::key("devices.serve_website_address_off"),
        None => LocalizedText::key("devices.serve_website_hint"),
    }
}

/// The localized label for a stored audience wire value.
///
/// An unrecognized value degrades to `private`'s label — the fail-CLOSED
/// reading, and the only safe one here: a value this binary does not know must
/// never be painted as `Public`, which would tell the user their folder is
/// world-readable (or that it is safe to publish into) on the strength of a
/// string it could not parse.
pub fn audience_label(value: &str) -> LocalizedText {
    LocalizedText::key(match value {
        AUDIENCE_PUBLIC => "devices.folder_audience_public",
        AUDIENCE_SHARED => "devices.folder_audience_shared",
        _ => "devices.folder_audience_private",
    })
}

// The two residency wire tokens are the WIRE crate's to spell — they are the
// values `fauna.folders.update` accepts and `FolderSummary::residency` carries,
// so `fauna-protocol` defines them beside the field and this crate re-exports
// rather than re-declaring. Re-exported (not merely used) because the picker
// below is every app's door to them, and an app should not need to learn which
// crate the token lives in to name the value its select just produced.
pub use fauna_protocol::folders::{RESIDENCY_FULL, RESIDENCY_METADATA_ONLY};
// Same reasoning for the audience tokens, which this crate's picker spells
// on every app's behalf.
pub use fauna_protocol::folders::{AUDIENCE_PRIVATE, AUDIENCE_PUBLIC, AUDIENCE_SHARED};

/// One `folder-nest-residency-select` option — the nest place's content
/// residency (folders re-model phase 5; `file-sync.md` § Content residency owns
/// the model). Mirrors [`AudienceOption`]: every app builds its picker from this
/// list. Both options are always selectable — the flip to metadata-only is
/// consent-gated in the app (`folder-residency-confirm`), not withheld.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ResidencyOption {
    /// [`RESIDENCY_FULL`] or [`RESIDENCY_METADATA_ONLY`] — what
    /// `fauna.folders.update` accepts.
    pub value: String,
    pub label: LocalizedText,
}

/// The residency picker: Full (default) then Metadata-only, in that order on
/// every app.
pub fn residency_options() -> Vec<ResidencyOption> {
    vec![
        ResidencyOption {
            value: RESIDENCY_FULL.to_string(),
            label: LocalizedText::key("devices.folder_residency_full"),
        },
        ResidencyOption {
            value: RESIDENCY_METADATA_ONLY.to_string(),
            label: LocalizedText::key("devices.folder_residency_metadata_only"),
        },
    ]
}

/// The residency a folder should be RENDERED as, given the wire value
/// (`FolderSummary::residency` — empty on a full folder — the nest omits it).
///
/// **Fail-closed to FULL**, the same direction every other reader of this
/// field takes (the seat's byte-upload gate, the nest's projection): only an
/// explicit, parsed `metadata_only` paints as metadata-only. A binary that
/// cannot parse the value must not tell the user the nest holds no copy of
/// their content — that claim is what the metadata-only confirm is gated on.
pub fn normalize_residency(value: &str) -> String {
    if value == RESIDENCY_METADATA_ONLY {
        RESIDENCY_METADATA_ONLY.to_string()
    } else {
        RESIDENCY_FULL.to_string()
    }
}

/// The localized label for a residency value — [`normalize_residency`]'s
/// fail-closed reading applied to the copy.
pub fn residency_label(value: &str) -> LocalizedText {
    LocalizedText::key(if value == RESIDENCY_METADATA_ONLY {
        "devices.folder_residency_metadata_only"
    } else {
        "devices.folder_residency_full"
    })
}

/// The hint beside `folder-nest-residency-select`, keyed on the same
/// normalized `current` the select paints so copy and control agree: a full
/// folder explains what the nest's copy buys (offline catch-up); a
/// metadata-only folder states the availability cost it accepted — content
/// moves only while a holding device is online, the nest cannot restore it,
/// metadata and snapshots still sync (the three consequences `file-sync.md`
/// § Content residency requires the opting UI to state).
pub fn residency_hint(current: &str) -> LocalizedText {
    LocalizedText::key(if current == RESIDENCY_METADATA_ONLY {
        "devices.folder_residency_metadata_only_hint"
    } else {
        "devices.folder_residency_hint"
    })
}

/// One `folder-share-role-select` / `folder-member-role-select` option —
/// a shared-set member's access grant (multi-writer Phase 1;
/// `ui/folders.md` § Sharing owns the access model). Mirrors
/// [`ConflictPolicyOption`]: every app builds its picker from this list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MemberAccessOption {
    /// The canonical wire/DB value (`"reader"` / `"writer"`) — what
    /// `fauna.folders.members.set_access` and the share-time grant persist,
    /// and what the cross-app `select(id, value)` e2e contract drives.
    pub value: String,
    pub label: LocalizedText,
}

/// The canonical member-access picker order — `reader` (the default; an absent
/// role row means reader) first.
const MEMBER_ACCESS_ORDER: [(&str, &str); 2] = [
    (MEMBER_ACCESS_READER, "devices.member_access_reader"),
    (MEMBER_ACCESS_WRITER, "devices.member_access_writer"),
];

/// The `reader` access wire value — the default, and what an **absent**
/// `folder_member_access` row means to the nest.
pub const MEMBER_ACCESS_READER: &str = "reader";
/// The `writer` access wire value. Spelled once here because more than the
/// picker catalog keys on it now: [`writer_grant_reach`] does too, and a second
/// literal is how the two would come to disagree.
pub const MEMBER_ACCESS_WRITER: &str = "writer";

/// The canonical member-access picker option list (wire value + i18n label
/// key), mirroring [`conflict_policy_options`]. The value list cannot drift
/// from what the nest's `folder_member_access.access` CHECK accepts.
pub fn member_access_options() -> Vec<MemberAccessOption> {
    MEMBER_ACCESS_ORDER
        .into_iter()
        .map(|(value, key)| MemberAccessOption {
            value: value.to_string(),
            label: LocalizedText::key(key),
        })
        .collect()
}

/// The localized label for a stored member-access wire value — an unknown or
/// absent value degrades to `reader`'s label (the fail-safe default the nest
/// applies to an absent role row), matching [`conflict_policy_label`]'s
/// degrade-to-default shape.
pub fn member_access_label(value: &str) -> LocalizedText {
    let key = MEMBER_ACCESS_ORDER
        .iter()
        .find(|(v, _)| *v == value)
        .map(|(_, k)| *k)
        .unwrap_or(MEMBER_ACCESS_ORDER[0].1);
    LocalizedText::key(key)
}

/// The `folder-writer-published-warning` copy for one member row, or `None`
/// when the grant carries no reach beyond the set (`ui/folders.md` § Sharing —
/// *A writer grant on a folder whose content is readable BEYOND its members
/// warns too*, ratified 2026-08-27).
///
/// A `writer` grant is described to the owner as permission to change files in
/// this folder. On a **published** folder it is more than that: the same grant
/// changes what people outside the set read, and on a website-serving folder
/// those changes land on the **owner's** site — the `web_files` rail is keyed
/// to the site owner whoever recorded the change, not to the recorder. The
/// word *Writer* does not convey that, so the share flow and the member row say
/// it.
///
/// **The condition is REACH, not the website toggle.** Content is readable
/// beyond the member set exactly when the audience is `public` (the corpus
/// rests unsealed and world-readable) or the folder is paywalled (served to
/// subscribers) — deliberately the same test [`website_serve_hint`] applies
/// when it calls the toggle inert, so the two can never drift apart. A
/// website-enabled folder that is neither public nor paywalled serves nobody,
/// and a `public` folder is world-readable through the follow flow whether or
/// not it is served as a site — which is why `website_enabled` is not an input
/// here.
///
/// Two sentences rather than one, because the two reaches are different facts —
/// the same reason [`audience_hint`] returns one key of several. Advisory only:
/// it never blocks the grant and never disables the picker, and it **stacks**
/// with the uncapped-quota warning on a folder that is both published and
/// uncapped (they name different consequences of one grant).
///
/// `audience` is the **normalized** value ([`normalize_audience`]) — the
/// fail-closed normalization is what keeps an unparseable column from ever
/// reaching the `public` arm and claiming a folder is world-readable on the
/// strength of a string this binary could not read.
pub fn writer_grant_reach(access: &str, audience: &str, paywalled: bool) -> Option<LocalizedText> {
    // An absent access row means reader (the nest's own fail-safe default), so
    // anything that is not exactly `writer` carries no write grant to warn
    // about — the same degrade direction as `member_access_label`.
    if access != MEMBER_ACCESS_WRITER {
        return None;
    }
    if audience == AUDIENCE_PUBLIC {
        return Some(LocalizedText::key("devices.writer_public_warning"));
    }
    if paywalled {
        return Some(LocalizedText::key("devices.writer_paywalled_warning"));
    }
    None
}

/// What a folder row's device-local binding section shows — the
/// `folder-location-*` rows and add form, and whether
/// `folder-access-revoked-warning` heads them (`file-sync.md` § Multi-writer
/// shared sets → *Revocation*).
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindingSection {
    /// The row carries its binding section at all.
    pub shown: bool,
    /// The section opens with `folder-access-revoked-warning`.
    pub revoked_warning: bool,
}

/// Decide [`BindingSection`] for one folder row.
///
/// `role` / `access` are the row's `FolderSummary` fields; `parked` is whether
/// any of this device's bindings to the set is parked (the agent's
/// `access_revoked` flag on the binding — the nest refused the write grant).
///
/// An owner row and a writer member's row bind; a reader's does not (a bound
/// folder whose edits cannot upload would break `file-sync.md`'s iron rule).
/// **A parked binding is shown whatever the row's access now reads**, and it is
/// what the warning is about: a demotion is exactly the change that turns the
/// row's access to `reader`, so a rule that keyed only on the current access
/// hid the warning — and the parked binding with it — on the first refresh after
/// the demotion, which is when the user looks. The park never touches the
/// user's files, so the binding stays visible and removable, and re-binding is
/// the way back (it re-runs the eager write-grant check).
pub fn binding_section(role: Option<&str>, access: Option<&str>, parked: bool) -> BindingSection {
    let is_member = role == Some("member");
    let writes = !is_member || access == Some(MEMBER_ACCESS_WRITER);
    BindingSection {
        shown: writes || parked,
        revoked_warning: is_member && parked,
    }
}

/// The localized `conflict-type-badge` text for one auto-resolve review row —
/// the single source every app renders (`folders.md:104`: the badge shows
/// the resolution *merged / latest-kept*, or the conflict type while still
/// unresolved). Derived from the three `ConflictSummary` fields that already
/// cross the FFI/wasm boundary (`resolution`, `resolved_at`, `conflict_type`),
/// so this is a pure client-side computation — no wire or at-rest change.
///
/// Replaces five hand-rolled per-app copies that had *diverged* on two axes:
/// android rendered the raw wire `conflict_type` even for a *resolved* row
/// (skipping the resolution arms every other app had), and web localized the
/// two known conflict types (`binary_copy` → `type_binary`,
/// `merge_markers` → `type_merge`) while the three natives leaked the raw wire
/// string despite defining the same i18n keys. This adopts web's richer shape
/// (priority #4 — the richest existing pattern) as canonical.
///
/// An unknown/live type (the engine's current `concurrent_edit`) has no i18n
/// key, so it degrades to the raw wire string via
/// [`LocalizedText::resolve`]'s missing-key fallback — matching the `_ => raw`
/// arm every per-app copy had.
pub fn conflict_badge_label(
    resolution: Option<&str>,
    resolved_at: Option<i64>,
    conflict_type: &str,
) -> LocalizedText {
    // The delete-vs-edit class renders its own badge whatever the resolution
    // stamp: "Latest kept" is technically true (the surviving edit is the
    // winning writer) but hides exactly the half the user needs — that a
    // delete they made was overruled and the file kept (file-sync.md §
    // Conflicts, delete-vs-edit — the row is informational: one candidate,
    // no re-point target).
    if conflict_type == "delete_declined" {
        return LocalizedText::key("devices.conflicts.delete_declined");
    }
    // A skipped catch-up change: unresolved until a later change on the path
    // lands, and never a "kept" outcome (`conflicts.md` § Skipped catch-up
    // changes reach the review list).
    if conflict_type == fauna_protocol::folders::CONFLICT_TYPE_CATCHUP_FAILED {
        return LocalizedText::key("devices.conflicts.type_catchup_failed");
    }
    match resolution {
        Some("merged") => LocalizedText::key("devices.conflicts.resolved_merged"),
        // Any other resolution stamp, or a chooser-resolved row (no
        // stamp but a `resolved_at`), reads as latest-kept.
        Some(_) => LocalizedText::key("devices.conflicts.resolved_latest_wins"),
        None if resolved_at.is_some() => {
            LocalizedText::key("devices.conflicts.resolved_latest_wins")
        }
        None => match conflict_type {
            "binary_copy" => LocalizedText::key("devices.conflicts.type_binary"),
            "merge_markers" => LocalizedText::key("devices.conflicts.type_merge"),
            "concurrent_edit" => LocalizedText::key("devices.conflicts.type_concurrent"),
            // Never the raw wire value: an unknown type used to ride out as
            // its own "key", so lookup's miss echoed "concurrent_edit" at the
            // human on every app (copy-audit, 2026-08-04).
            _ => LocalizedText::key("devices.conflicts.type_other"),
        },
    }
}

/// Parse the `folder-include-paths` / `folder-exclude-paths` edit field into
/// the typed list `fauna.folders.update` takes: comma-split, whitespace
/// trimmed, empties dropped.
///
/// **Always returns a list — an emptied field parses to `[]`, never an absent
/// field.** The update wire treats an absent path field as *leave unchanged*
/// and `[]` as *clear the filter* (an empty include list disables the
/// whitelist — the engine then syncs everything), so `[]` is what "clear all
/// paths and save" must send. Replaces five hand-rolled per-app copies, one
/// of which (android) had diverged into sending `null` for an emptied field,
/// silently dropping the user's clear.
pub fn parse_paths_field(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Render stored selective-sync paths back into the single-line edit field
/// [`parse_paths_field`] reads: comma+space-joined; an absent or empty list
/// renders as `""`. The inverse half of the same lift.
pub fn join_paths_field(paths: Option<&[String]>) -> String {
    paths.map(|v| v.join(", ")).unwrap_or_default()
}

/// The Photo Library preset's snapshot policy (7 snapshots / 30 days, the
/// former backup-type default) — stamped **explicitly** on the preset's create
/// (`ui/folders.md` § Photo backup). A plain wizard create stamps none: the
/// nest place's policy rests unset until the owner edits `folder-nest-*`.
pub const PHOTO_LIBRARY_RETENTION: RetentionPolicy = RetentionPolicy {
    max_snapshots: 7,
    max_age_days: 30,
};

/// Internal — the full wizard state. In-memory only; not persisted, not exposed
/// over UniFFI (clients read snapshots through getters). Mirrors
/// `fauna_onboarding_machine::state::State`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct State {
    pub step: FolderWizardStep,
    pub name: String,
    pub devices: Vec<WizardDevice>,
    /// The retention policy the create stamps — `None` for every plain wizard
    /// create; only a preset sets one ([`PHOTO_LIBRARY_RETENTION`]).
    pub retention: Option<RetentionPolicy>,
    /// The user's global default conflict policy (`"auto"` |
    /// `"latest_wins_always"`), injected by the client glue from
    /// the `fauna.state.sync-prefs` default conflict policy via
    /// `set_default_conflict_policy` right after `open_wizard`. Stamped onto the
    /// create request so a new set starts on the preferred policy. `None` = no
    /// preference (the nest column default, `auto`).
    pub default_conflict_policy: Option<String>,
    /// `submit()` lifecycle.
    pub submit_phase: SubmitPhase,
    /// True once the `POST /folders` create succeeded (so a partial-failure
    /// retry doesn't 409 on re-create).
    pub created: bool,
    /// Device ids that still need enrolling. Seeded with every selected device
    /// after a successful create; a device id is removed once its member-add
    /// succeeds. After a partial failure this holds exactly the devices to
    /// retry (so a retry never re-adds an already-enrolled device). Empty
    /// before create and after full success.
    pub pending_member_ids: Vec<String>,
    /// Structured submit error for the review surface. `None` until a failure.
    pub error: Option<LocalizedText>,
}

impl State {
    pub(crate) fn new(devices: Vec<WizardDevice>) -> Self {
        Self {
            step: FolderWizardStep::Name,
            name: String::new(),
            devices,
            retention: None,
            default_conflict_policy: None,
            submit_phase: SubmitPhase::Idle,
            created: false,
            pending_member_ids: Vec::new(),
            error: None,
        }
    }

    /// Whether the name step's Continue is enabled (non-empty, trimmed name).
    pub(crate) fn name_valid(&self) -> bool {
        !self.name.trim().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_policy_options_match_the_wire_catalog() {
        // The catalog derives from `fauna_core::format::ConflictPolicy` (the
        // wire/DB enum), so the picker's value list cannot drift from the
        // values the nest column accepts.
        let opts = conflict_policy_options();
        let values: Vec<&str> = opts.iter().map(|o| o.value.as_str()).collect();
        assert_eq!(values, vec!["auto", "latest_wins_always"]);
        let keys: Vec<&str> = opts.iter().map(|o| o.label.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "devices.conflict_policy_auto",
                "devices.conflict_policy_latest_wins",
            ]
        );
    }

    #[test]
    fn binding_section_binds_owners_and_writers_and_never_a_plain_reader() {
        let owner = binding_section(Some("owner"), None, false);
        assert_eq!(
            owner,
            BindingSection {
                shown: true,
                revoked_warning: false
            }
        );
        // A row with no role reads as the caller's own.
        assert!(binding_section(None, None, false).shown);
        let writer = binding_section(Some("member"), Some(MEMBER_ACCESS_WRITER), false);
        assert_eq!(
            writer,
            BindingSection {
                shown: true,
                revoked_warning: false
            }
        );
        // Absent access is a reader — the nest's own fail-safe default.
        for access in [Some(MEMBER_ACCESS_READER), None, Some("Writer")] {
            assert_eq!(
                binding_section(Some("member"), access, false),
                BindingSection {
                    shown: false,
                    revoked_warning: false
                },
                "{access:?}"
            );
        }
    }

    /// The demotion case: the park lands, and then the row's own access reads
    /// `reader`. The warning and the parked binding must survive that refresh —
    /// keying the section on the current access alone is the bug this pins.
    #[test]
    fn a_parked_binding_is_shown_and_warned_about_whatever_the_access_now_reads() {
        for access in [Some(MEMBER_ACCESS_READER), None, Some(MEMBER_ACCESS_WRITER)] {
            assert_eq!(
                binding_section(Some("member"), access, true),
                BindingSection {
                    shown: true,
                    revoked_warning: true
                },
                "{access:?}"
            );
        }
        // An owner's grant cannot be withdrawn, so its row never says so.
        assert!(!binding_section(Some("owner"), None, true).revoked_warning);
    }

    #[test]
    fn writer_grant_reach_warns_only_on_a_writer_whose_folder_reaches_past_its_members() {
        // The whole point of the warning: a writer on a PUBLISHED folder is
        // being handed the power to change what outsiders read.
        assert_eq!(
            writer_grant_reach(MEMBER_ACCESS_WRITER, AUDIENCE_PUBLIC, false)
                .expect("a writer on a public folder warns")
                .key,
            "devices.writer_public_warning"
        );
        assert_eq!(
            writer_grant_reach(MEMBER_ACCESS_WRITER, AUDIENCE_SHARED, true)
                .expect("a writer on a paywalled folder warns")
                .key,
            "devices.writer_paywalled_warning"
        );

        // A reader changes nothing, so there is nothing to warn about however
        // published the folder is — including the unknown/absent access value,
        // which the nest reads as reader.
        assert!(writer_grant_reach(MEMBER_ACCESS_READER, AUDIENCE_PUBLIC, true).is_none());
        assert!(writer_grant_reach("", AUDIENCE_PUBLIC, true).is_none());
        assert!(writer_grant_reach("Writer", AUDIENCE_PUBLIC, true).is_none());

        // A writer on a folder nobody outside the set can read is the ordinary
        // case the word "Writer" already describes — silence is correct.
        assert!(writer_grant_reach(MEMBER_ACCESS_WRITER, AUDIENCE_SHARED, false).is_none());
        assert!(writer_grant_reach(MEMBER_ACCESS_WRITER, AUDIENCE_PRIVATE, false).is_none());
    }

    #[test]
    fn writer_grant_reach_and_website_serve_hint_agree_on_what_reaches_outsiders() {
        // The two decisions share one test by construction: `website_serve_hint`
        // calls the toggle INERT on exactly the folders where a writer grant is
        // ordinary. If a future edit moves one boundary and not the other, the
        // UI would call a folder unserved while warning that its writer can
        // change what the public sees (or the reverse). Address-flag state is
        // irrelevant to the pairing — it only picks WHICH served-wording the
        // hint uses — so the property must hold for all three of its values.
        for &(audience, paywalled) in &[
            (AUDIENCE_PUBLIC, false),
            (AUDIENCE_PUBLIC, true),
            (AUDIENCE_SHARED, true),
            (AUDIENCE_SHARED, false),
            (AUDIENCE_PRIVATE, false),
            (AUDIENCE_PRIVATE, true),
        ] {
            let serves_nobody = website_serve_hint(audience, paywalled, Some(true)).key
                == "devices.serve_website_needs_audience";
            let warns = writer_grant_reach(MEMBER_ACCESS_WRITER, audience, paywalled).is_some();
            assert_eq!(
                serves_nobody, !warns,
                "reach disagreement for audience={audience} paywalled={paywalled}"
            );
            for flag in [Some(false), None] {
                assert_eq!(
                    website_serve_hint(audience, paywalled, flag).key
                        == "devices.serve_website_needs_audience",
                    serves_nobody,
                    "the address flag must not move the reach boundary"
                );
            }
        }
    }

    #[test]
    fn writer_grant_reach_never_warns_from_an_unparseable_audience() {
        // `normalize_audience` is fail-closed — nothing unreadable resolves to
        // `public` — and this function is its consumer, so an unparseable audience
        // (empty string) can never make the UI claim a
        // folder is world-readable. Pinned end-to-end through the normalizer,
        // since that is how every app feeds this.
        for raw in ["", "PUBLIC", "world", "publik"] {
            for bound in [true, false] {
                let normalized = normalize_audience(raw, bound);
                assert!(
                    writer_grant_reach(MEMBER_ACCESS_WRITER, &normalized, false).is_none(),
                    "unparseable audience {raw:?} (bound={bound}) must not warn"
                );
            }
        }
    }

    #[test]
    fn the_access_catalog_spells_its_two_values_through_the_shared_constants() {
        // The constants exist so `writer_grant_reach` and the picker cannot
        // drift; this pins that the catalog really is built from them.
        let values: Vec<String> = member_access_options()
            .into_iter()
            .map(|o| o.value)
            .collect();
        assert_eq!(values, vec![MEMBER_ACCESS_READER, MEMBER_ACCESS_WRITER]);
    }

    #[test]
    fn member_access_options_match_the_wire_catalog() {
        // The catalog's value list matches the nest's
        // `folder_member_access.access` CHECK ('reader' | 'writer'); reader
        // (the absent-row default) is first.
        let opts = member_access_options();
        let values: Vec<&str> = opts.iter().map(|o| o.value.as_str()).collect();
        assert_eq!(values, vec!["reader", "writer"]);
        let keys: Vec<&str> = opts.iter().map(|o| o.label.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "devices.member_access_reader",
                "devices.member_access_writer",
            ]
        );
    }

    #[test]
    fn member_access_label_resolves_both_and_degrades_unknown_to_reader() {
        assert_eq!(
            member_access_label("writer").key,
            "devices.member_access_writer"
        );
        assert_eq!(
            member_access_label("reader").key,
            "devices.member_access_reader"
        );
        // Unknown/absent degrades to reader — the fail-safe absent-row default.
        assert_eq!(
            member_access_label("bogus").key,
            "devices.member_access_reader"
        );
    }

    #[test]
    fn conflict_policy_label_resolves_both_values_and_degrades_unknown_to_auto() {
        assert_eq!(
            conflict_policy_label("auto").key,
            "devices.conflict_policy_auto"
        );
        assert_eq!(
            conflict_policy_label("latest_wins_always").key,
            "devices.conflict_policy_latest_wins"
        );
        // Unknown values degrade to Auto, matching `ConflictPolicy::from_wire`
        // (and the `_ =>` arm every per-app copy this helper replaces had).
        assert_eq!(
            conflict_policy_label("bogus").key,
            "devices.conflict_policy_auto"
        );
    }

    #[test]
    fn conflict_badge_label_shows_the_resolution_when_resolved() {
        // folders.md:104 — `conflict-type-badge` shows the resolution
        // (merged / latest-kept) once a conflict is auto-resolved.
        assert_eq!(
            conflict_badge_label(Some("merged"), Some(10), "concurrent_edit").key,
            "devices.conflicts.resolved_merged"
        );
        assert_eq!(
            conflict_badge_label(Some("latest_wins"), Some(10), "concurrent_edit").key,
            "devices.conflicts.resolved_latest_wins"
        );
        // A chooser-resolved row carries no `resolution` stamp but a
        // `resolved_at` — it still reads as latest-kept, never the raw type
        // (the arm every native app already had; the bug android lacked).
        assert_eq!(
            conflict_badge_label(None, Some(10), "concurrent_edit").key,
            "devices.conflicts.resolved_latest_wins"
        );
        // The delete-vs-edit class (file-sync.md § Conflicts, ratified
        // 2026-07-29) keeps its own badge through resolution: "Latest kept"
        // is technically true but hides the half the user needs — that a
        // delete they made was overruled.
        assert_eq!(
            conflict_badge_label(Some("latest_wins"), Some(10), "delete_declined").key,
            "devices.conflicts.delete_declined"
        );
        // The degraded (unresolved-fallback) row of the same class reads the
        // same — the class, not the ladder rung, is what the user needs.
        assert_eq!(
            conflict_badge_label(None, None, "delete_declined").key,
            "devices.conflicts.delete_declined"
        );
    }

    /// A skipped catch-up change gets its own badge, never the generic
    /// `type_other` (`conflicts.md` § Skipped catch-up changes reach the
    /// review list).
    #[test]
    fn conflict_badge_label_names_a_skipped_catch_up_change() {
        assert_eq!(
            conflict_badge_label(None, None, "catchup_failed").key,
            "devices.conflicts.type_catchup_failed"
        );
    }

    #[test]
    fn conflict_badge_label_localizes_the_type_when_unresolved_and_never_echoes_the_wire() {
        // Unresolved rows show the conflict *type*, always through an i18n
        // key. The old `_ => raw` arm (inherited from every per-app copy)
        // echoed the wire string at the human — live users read
        // "concurrent_edit" as a badge (copy-audit, 2026-08-04). Unknown
        // future types take the generic key rather than leaking their
        // spelling. The e2e's unresolved-row detection is unaffected: it
        // allowlists the RESOLVED badges (`sync_seats.unresolved_review_rows`),
        // so any type badge — localized or not — still reads as unresolved.
        assert_eq!(
            conflict_badge_label(None, None, "binary_copy").key,
            "devices.conflicts.type_binary"
        );
        assert_eq!(
            conflict_badge_label(None, None, "merge_markers").key,
            "devices.conflicts.type_merge"
        );
        assert_eq!(
            conflict_badge_label(None, None, "concurrent_edit").key,
            "devices.conflicts.type_concurrent"
        );
        assert_eq!(
            conflict_badge_label(None, None, "some_future_type").key,
            "devices.conflicts.type_other"
        );
    }

    #[test]
    fn parse_paths_field_splits_trims_and_drops_empties() {
        assert_eq!(
            parse_paths_field("docs, src/lib , ,photos/2026,"),
            vec!["docs", "src/lib", "photos/2026"]
        );
        // A single path with no comma passes through trimmed.
        assert_eq!(parse_paths_field("  docs  "), vec!["docs"]);
    }

    #[test]
    fn parse_paths_field_of_an_emptied_field_is_the_empty_list_never_absent() {
        // The `fauna.folders.update` wire treats an *absent* path field as
        // "leave unchanged" and `[]` as "clear the filter" (sync everything —
        // an empty include list disables the whitelist). So clearing the edit
        // field MUST produce `[]`, never an absent field: this is the arm
        // android's hand-rolled copy got wrong (`.ifEmpty { null }` made
        // "clear all paths and save" a silent no-op).
        assert_eq!(parse_paths_field(""), Vec::<String>::new());
        assert_eq!(parse_paths_field("  ,  , "), Vec::<String>::new());
    }

    #[test]
    fn join_paths_field_renders_comma_space_and_empty_for_absent() {
        let paths = vec!["docs".to_string(), "src/lib".to_string()];
        assert_eq!(join_paths_field(Some(&paths)), "docs, src/lib");
        assert_eq!(join_paths_field(None), "");
        assert_eq!(join_paths_field(Some(&[])), "");
    }

    #[test]
    fn paths_field_round_trips() {
        let stored = vec!["docs".to_string(), "photos/2026".to_string()];
        assert_eq!(parse_paths_field(&join_paths_field(Some(&stored))), stored);
    }

    #[test]
    fn retention_serializes_to_nest_shape() {
        let json = serde_json::to_value(PHOTO_LIBRARY_RETENTION).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"max_snapshots": 7, "max_age_days": 30})
        );
    }

    #[test]
    fn fresh_state_matches_web_defaults() {
        let s = State::new(vec![]);
        assert_eq!(s.step, FolderWizardStep::Name);
        assert_eq!(s.retention, None, "a plain create stamps no retention");
        assert!(!s.name_valid());
    }

    // ── Audience picker (phase 4 slice 4d) ────────────────────────────────

    /// An UNBOUND folder chooses between private and public. `shared` is absent
    /// because the nest refuses it while unbound — an option that fails on click
    /// is worse than an absent one.
    #[test]
    fn an_unbound_folder_is_offered_private_and_public() {
        for current in ["private", "public"] {
            let opts = audience_options(false, current);
            let values: Vec<&str> = opts.iter().map(|o| o.value.as_str()).collect();
            assert_eq!(values, ["private", "public"]);
            assert!(opts.iter().all(|o| o.selectable), "current={current}");
        }
    }

    /// A BOUND, not-public folder renders `shared` as its current state — NOT
    /// selectable, because bound-ness is entered through the share flow and
    /// nowhere else — and may still go public. `private` is withheld: the nest
    /// refuses it while bound, and the honest repair is to remove the sharing
    /// first.
    #[test]
    fn a_bound_folder_renders_shared_unselectable_and_withholds_private() {
        let opts = audience_options(true, "shared");
        let values: Vec<&str> = opts.iter().map(|o| o.value.as_str()).collect();
        assert_eq!(values, ["shared", "public"]);
        assert!(!opts[0].selectable, "shared is a state, not a destination");
        assert!(
            opts[1].selectable,
            "a bound folder may still be declassified"
        );
    }

    /// a BOUND folder currently `public` offers `shared` as a REAL
    /// destination: the one legal exit from its public window (the nest accepts
    /// `→shared` exactly while bound), and the pick is what re-seals the corpus
    /// for its members. Before this, the picker offered a bound folder exactly
    /// one selectable option and it was the irreversible one — a one-way
    /// declassification of every member's content.
    #[test]
    fn a_bound_public_folder_offers_shared_as_the_flip_back() {
        let opts = audience_options(true, "public");
        let values: Vec<&str> = opts.iter().map(|o| o.value.as_str()).collect();
        assert_eq!(values, ["shared", "public"]);
        assert!(
            opts[0].selectable,
            "shared is the flip-back exit while the folder is public"
        );
        assert!(opts[1].selectable);
    }

    /// The hint rides the same (bound, current) inputs as the picker, so the
    /// two cannot disagree: the bound-public state explains its one exit, the
    /// bound state points at the sharing section, the unbound state explains
    /// the private/public choice.
    #[test]
    fn the_audience_hint_matches_what_the_picker_offers() {
        assert_eq!(
            audience_hint(true, "public"),
            LocalizedText::key("devices.folder_audience_public_bound_hint")
        );
        assert_eq!(
            audience_hint(true, "shared"),
            LocalizedText::key("devices.folder_audience_shared_hint")
        );
        for current in ["private", "public"] {
            assert_eq!(
                audience_hint(false, current),
                LocalizedText::key("devices.folder_audience_hint"),
                "current={current}"
            );
        }
    }

    /// ⚠ The fail-closed floor, asserted in the only direction that can hurt: no
    /// unparseable value may EVER resolve to `public`. A binary that cannot read
    /// the column must not tell the user their folder is world-readable, nor
    /// invite them to publish into one it cannot vouch for.
    #[test]
    fn an_unknown_audience_never_resolves_to_public() {
        for raw in ["", "PUBLIC", "world", "publick", "nonsense", "private"] {
            assert_ne!(
                normalize_audience(raw, false),
                "public",
                "{raw:?} must not read as public"
            );
            assert_ne!(
                normalize_audience(raw, true),
                "public",
                "{raw:?} must not read as public"
            );
        }
        // Only the exact wire value does.
        assert_eq!(normalize_audience("public", false), "public");
        assert_eq!(normalize_audience("public", true), "public");
    }

    /// The normalized value is always one the picker actually offers — a select
    /// showing a value outside its own option set is an unpaintable state, and
    /// an unparseable value (an empty string, say) reaches it. The options
    /// are built over the normalized current value, exactly as every render
    /// site calls the pair.
    #[test]
    fn every_normalized_audience_is_an_offered_option() {
        for bound in [false, true] {
            for raw in ["", "private", "shared", "public", "garbage"] {
                let resolved = normalize_audience(raw, bound);
                let offered: Vec<String> = audience_options(bound, &resolved)
                    .into_iter()
                    .map(|o| o.value)
                    .collect();
                assert!(
                    offered.contains(&resolved),
                    "bound={bound}, raw={raw:?} resolved to {resolved:?}, \
                     which is not among {offered:?}"
                );
            }
        }
    }

    /// An unbound row claiming `shared` is as unparseable as an empty string —
    /// `shared` is only coherent for a bound folder — so it falls to the same
    /// private floor rather than being echoed back.
    #[test]
    fn shared_on_an_unbound_folder_falls_to_private() {
        assert_eq!(normalize_audience("shared", false), "private");
        assert_eq!(normalize_audience("shared", true), "shared");
    }

    /// The label follows the same fail-closed reading as the value, so the two
    /// cannot disagree about what the row is.
    #[test]
    fn the_audience_label_is_fail_closed_too() {
        assert_eq!(
            audience_label("public"),
            LocalizedText::key("devices.folder_audience_public")
        );
        for raw in ["", "garbage", "private"] {
            assert_eq!(
                audience_label(raw),
                LocalizedText::key("devices.folder_audience_private"),
                "{raw:?}"
            );
        }
    }

    /// Residency is fail-closed to FULL at every reader: only the exact
    /// `metadata_only` value paints as metadata-only; empty (a full
    /// folder), garbage, and the explicit `full` all paint as full — and the
    /// label + hint follow the same reading, so the row cannot claim the nest
    /// holds no copy on the strength of a value it could not parse.
    #[test]
    fn residency_is_fail_closed_to_full_and_its_copy_follows() {
        assert_eq!(
            normalize_residency(RESIDENCY_METADATA_ONLY),
            RESIDENCY_METADATA_ONLY
        );
        assert_eq!(
            residency_label(RESIDENCY_METADATA_ONLY),
            LocalizedText::key("devices.folder_residency_metadata_only")
        );
        assert_eq!(
            residency_hint(RESIDENCY_METADATA_ONLY),
            LocalizedText::key("devices.folder_residency_metadata_only_hint")
        );
        for raw in ["", "full", "garbage", "METADATA_ONLY", "metadata-only"] {
            assert_eq!(normalize_residency(raw), RESIDENCY_FULL, "{raw:?}");
            assert_eq!(
                residency_label(raw),
                LocalizedText::key("devices.folder_residency_full"),
                "{raw:?}"
            );
            assert_eq!(
                residency_hint(raw),
                LocalizedText::key("devices.folder_residency_hint"),
                "{raw:?}"
            );
        }
    }

    /// Every normalized residency is an offered option (the select's value must
    /// be paintable), Full leads, and both are plain picks — the consent gate
    /// is the app's confirm, never a withheld option.
    #[test]
    fn every_normalized_residency_is_an_offered_option() {
        let values: Vec<String> = residency_options().into_iter().map(|o| o.value).collect();
        assert_eq!(values, vec![RESIDENCY_FULL, RESIDENCY_METADATA_ONLY]);
        for raw in ["", "garbage", RESIDENCY_METADATA_ONLY] {
            let normalized = normalize_residency(raw);
            assert!(values.contains(&normalized), "{raw:?} → {normalized}");
        }
    }

    /// The website-toggle hint's tri-state: the audience wording while nothing
    /// can serve, then the live/off/unknown split on the address flag — and
    /// the degrade direction (unknown must never read as live).
    #[test]
    fn website_serve_hint_keys_on_audience_then_the_address_flag() {
        // Not public, not paywalled: the audience wording, whatever the flag —
        // there is nothing to reach regardless of the address.
        for flag in [Some(true), Some(false), None] {
            assert_eq!(
                website_serve_hint("private", false, flag),
                LocalizedText::key("devices.serve_website_needs_audience"),
                "{flag:?}"
            );
        }
        // Paywalled counts as a readable audience, exactly like public.
        assert_eq!(
            website_serve_hint("private", true, Some(true)),
            LocalizedText::key("devices.serve_website_live")
        );
        assert_eq!(
            website_serve_hint("public", false, Some(true)),
            LocalizedText::key("devices.serve_website_live")
        );
        // The one misleading case the tri-state exists for: published here,
        // reachable by nobody.
        assert_eq!(
            website_serve_hint("public", false, Some(false)),
            LocalizedText::key("devices.serve_website_address_off")
        );
        // Unknown degrades to the combined wording — never to "live".
        assert_eq!(
            website_serve_hint("public", false, None),
            LocalizedText::key("devices.serve_website_hint")
        );
    }
}
