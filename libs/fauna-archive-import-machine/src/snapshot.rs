//! What the per-app `archive-import` page renders and the actions it
//! dispatches — `archive-import.md` § The wizard and its machine, steps 1–6.
//! Element IDs are the proposed `archive-import-*` set (rule-A approval
//! pending; slice 4 lands them in ui.yaml).
//!
//! Every type here is FFI-flat — plain scalars, `String`, `Option` and `Vec`
//! of the same — so one snapshot crosses UniFFI (native) and wasm-bindgen
//! (web) unchanged and all 7 apps render from the identical shape
//! (priorities #1–#3).

use fauna_core::localized::LocalizedText;
use serde::{Deserialize, Serialize};

/// Which wizard screen is showing (`archive-import.md` § The wizard and its
/// machine, the six numbered steps).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ArchiveImportStep {
    /// Step 1 — `archive-import-source-picker` + `archive-import-source-help`.
    Source,
    /// Step 2 — `archive-import-archive-path` + `archive-import-archive-summary`.
    Archive,
    /// Step 3 — the `archive-import-scope-*` controls.
    Scope,
    /// Step 4 — `archive-import-confirm-summary` + `archive-import-start-button`.
    Confirm,
    /// Step 5 — `archive-import-progress-*`, the category list and the error log.
    Progress,
    /// Step 6 — `archive-import-done-summary` and its deep links.
    Done,
}

/// Step 1's picker. Mirrors the platforms `fauna_archive::Platform` names
/// as a plain enum so the snapshot stays FFI-flat — and so that enum's open
/// arm (a newer parser's platform, carried) never becomes a case an app
/// renders.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ArchiveSourceKind {
    Facebook,
    Instagram,
}

impl ArchiveSourceKind {
    pub fn platform(self) -> fauna_archive::Platform {
        match self {
            Self::Facebook => fauna_archive::Platform::Facebook,
            Self::Instagram => fauna_archive::Platform::Instagram,
        }
    }

    /// The picker entry for a platform; `None` for one this build cannot
    /// name (a carried `Platform::Other`), which no picker offers.
    pub fn from_platform(p: &fauna_archive::Platform) -> Option<Self> {
        match p {
            fauna_archive::Platform::Facebook => Some(Self::Facebook),
            fauna_archive::Platform::Instagram => Some(Self::Instagram),
            fauna_archive::Platform::Other(_) => None,
        }
    }

    /// The canonical label (`Platform::label`, pinned equal by
    /// `vocabulary_tests`).
    pub fn label(self) -> &'static str {
        match self {
            Self::Facebook => "Facebook",
            Self::Instagram => "Instagram",
        }
    }

    /// Where the export is requested (`Platform::export_request_url`).
    pub fn export_request_url(self) -> &'static str {
        self.platform()
            .export_request_url()
            .expect("every picker entry is a platform this build names")
    }
}

/// Step 3's audience-mode switch (`archive-import.md` § Audience mapping).
/// `Original` keeps each record's platform audience; `OnlyMe` overrides the
/// whole mapping to the reserved owner-only tier.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum AudienceMode {
    Original,
    OnlyMe,
}

/// The order Step 1's platform picker offers its entries in (and the index an
/// app's select widget maps back through). One owner for every app.
pub const SOURCE_KINDS: [ArchiveSourceKind; 2] =
    [ArchiveSourceKind::Facebook, ArchiveSourceKind::Instagram];

/// The order Step 3's audience-mode switch offers its entries in.
pub const AUDIENCE_MODES: [AudienceMode; 2] = [AudienceMode::Original, AudienceMode::OnlyMe];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ArchiveImportStatus {
    Idle,
    Loading,
    Working,
}

/// The run's state on the Progress/Done screens.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum RunState {
    Running,
    Paused,
    Cancelled,
    Completed,
    Errored,
}

/// One row of `archive-import-scope-categories` / the progress list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CategoryRow {
    /// `fauna_archive::Category::token()`.
    pub token: String,
    pub count: u64,
    /// Whether the category becomes Fauna content (posts, albums, events);
    /// the rest are model-only in phase one and render unselectable.
    pub importable: bool,
    pub selected: bool,
    pub imported: u64,
    pub skipped: u64,
}

/// Step 2's `archive-import-archive-summary`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ArchiveSummaryView {
    pub platform_label: String,
    pub owner_display_name: String,
    /// Epoch micros, `0` when the archive carries no dated record.
    pub first_at: u64,
    pub last_at: u64,
    pub media_bytes: u64,
    pub archive_bytes: u64,
    /// Posts + albums the export labels with an audience — imported at it.
    pub known_audience: u64,
    /// Posts + albums with no recorded audience — imported owner-only
    /// (`archive-import.md` § Audience mapping: `Unknown` → owner-only).
    pub unknown_audience: u64,
}

/// Step 6's profile prefill offer (`archive-import.md` § What each category
/// becomes, the Profile row: "offered as a one-tap prefill at Done, never
/// applied silently") — the archive's own profile, read from the folder's
/// `model/profile.cbor` when the run completes. What
/// [`ArchiveImportAction::ApplyArchiveProfile`] writes onto the user's own
/// profile, and nothing until that tap.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ArchiveProfileView {
    pub display_name: String,
    pub bio: Option<String>,
    pub links: Vec<String>,
    /// The export carries a profile picture (the apply uploads it).
    pub has_picture: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ArchiveImportSnapshot {
    pub step: ArchiveImportStep,
    pub source_kind: ArchiveSourceKind,
    /// Step 1's deep link: the page on the chosen platform where the export
    /// is requested (`ArchiveSourceKind::export_request_url`; derived — the
    /// machine fills it on every read).
    pub source_export_url: String,
    /// `archive-import-archive-path` — path entry on tui/desktop; web's file
    /// input writes the picked name here for display.
    pub archive_path: String,
    pub summary: Option<ArchiveSummaryView>,
    pub categories: Vec<CategoryRow>,
    pub audience_mode: AudienceMode,
    /// `archive-import-scope-date-from` / `-to`, `YYYY-MM-DD` or empty.
    pub date_from: String,
    pub date_to: String,
    /// `archive-import-confirm-summary`: records that will be re-authored +
    /// bytes that will be uploaded (raw zip + media), from the scope.
    pub confirm_records: u64,
    pub confirm_bytes: u64,
    /// Steps 5–6.
    pub folder_name: Option<String>,
    pub run_state: Option<RunState>,
    pub total: u64,
    pub imported: u64,
    pub skipped: u64,
    pub errored: u64,
    pub current_category: Option<String>,
    /// Step 5's elapsed time and estimate, whole seconds: the run's active
    /// time as this app watched it (it stops while paused), and the time
    /// left at the rate watched — `None` until a record has settled under
    /// this app's clock. Derived on every read (`timing.rs`).
    pub elapsed_secs: u64,
    pub eta_secs: Option<u64>,
    /// `archive-import-error-log`, newest last.
    pub skip_log: Vec<String>,
    /// `Some(false)` ⇒ the nest predates hidden tiers: non-public categories
    /// import nothing and the scope step says so (§ Audience mapping).
    pub nest_supports_hidden_tiers: Option<bool>,
    /// A resumable import found in a folder at hydrate; the Archive step asks
    /// for the archive again unless the folder's raw copy can be read.
    pub resume_available: bool,
    pub status: ArchiveImportStatus,
    pub error: Option<String>,
    /// Phase two (ruling 11): comments and reactions that came back as this
    /// user's own replies and reactions on linked contacts' posts, and the
    /// ones no post could be matched to — the Done step's one added line.
    /// Set by [`crate::ArchiveImportMachine::set_link_counts`] from
    /// [`crate::link::ArchiveLinks::counts`]; `0` before any link exists.
    pub link_claimed: u64,
    pub link_unmatched: u64,
    /// The comments on the owner's own posts no rung of ruling 5's ladder
    /// placed under a post (§ What each category becomes → *Placement under
    /// the owner's own post*) — the Done step's "N comments could not be
    /// placed under a post" line. Counted from the folders when a run
    /// finishes; `0` before.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = 0))]
    pub comments_unplaced: u64,
    /// Step 6's profile prefill offer — `None` before a run completes, or
    /// when the archive carries no profile; the button paints disabled then.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub archive_profile: Option<ArchiveProfileView>,
    /// Set once the offer was applied: the button paints disabled, so a
    /// second tap never re-publishes the same profile.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    pub profile_applied: bool,
}

impl ArchiveImportSnapshot {
    /// The page as it paints before `hydrate` has answered: step 1, nothing
    /// known about the nest or any archive.
    pub fn empty() -> Self {
        Self {
            step: ArchiveImportStep::Source,
            source_kind: ArchiveSourceKind::Facebook,
            source_export_url: ArchiveSourceKind::Facebook.export_request_url().to_string(),
            archive_path: String::new(),
            summary: None,
            categories: Vec::new(),
            audience_mode: AudienceMode::Original,
            date_from: String::new(),
            date_to: String::new(),
            confirm_records: 0,
            confirm_bytes: 0,
            folder_name: None,
            run_state: None,
            total: 0,
            imported: 0,
            skipped: 0,
            errored: 0,
            current_category: None,
            elapsed_secs: 0,
            eta_secs: None,
            skip_log: Vec::new(),
            nest_supports_hidden_tiers: None,
            resume_available: false,
            status: ArchiveImportStatus::Idle,
            error: None,
            link_claimed: 0,
            link_unmatched: 0,
            comments_unplaced: 0,
            archive_profile: None,
            profile_applied: false,
        }
    }
}

impl Default for ArchiveImportSnapshot {
    fn default() -> Self {
        Self::empty()
    }
}

// ── the page's vocabularies ──────────────────────────────────────────────
//
// Every app paints the same few token → word maps on this page. They live
// here once, returned as `LocalizedText` so each app resolves them through
// its own i18n runtime (the `export_format_label` /
// `import_source_kind_label` precedent in `fauna-client-mail-settings`), and
// no app re-spells a map tui already had.

/// Step 1's picker option: the platform's canonical name — a brand, never
/// translated, so a plain string ([`ArchiveSourceKind::label`]); the picker
/// round-trips it as its value.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn archive_import_source_kind_label(kind: ArchiveSourceKind) -> String {
    kind.label().to_string()
}

/// Step 1's `archive-import-source-help`: how to request the export on the
/// chosen platform.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn archive_import_source_help(kind: ArchiveSourceKind) -> LocalizedText {
    LocalizedText::key(match kind {
        ArchiveSourceKind::Facebook => "archive_import.source_help_facebook",
        ArchiveSourceKind::Instagram => "archive_import.source_help_instagram",
    })
}

/// The word for a [`CategoryRow::token`] (`fauna_archive::Category::token`)
/// on the Scope and Progress rows. A token this build has no word for (a
/// newer machine's category) resolves to the token itself — every resolver
/// falls back to the key when the lookup misses — so the row shows something
/// rather than nothing.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn archive_import_category_label(token: &str) -> LocalizedText {
    let key = match token {
        "posts" => "archive_import.category_posts",
        "albums" => "archive_import.category_albums",
        "comments" => "archive_import.category_comments",
        "reactions" => "archive_import.category_reactions",
        "events" => "archive_import.category_events",
        "groups" => "archive_import.category_groups",
        "friends" => "archive_import.category_friends",
        "threads" => "archive_import.category_threads",
        "messages" => "archive_import.category_messages",
        "profile" => "archive_import.category_profile",
        other => other,
    };
    LocalizedText::key(key)
}

/// Step 5's state word inside `archive-import-progress-summary`; no run state
/// yet reads as running (the Progress screen is only shown once Start
/// landed).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn archive_import_run_state_label(state: Option<RunState>) -> LocalizedText {
    LocalizedText::key(match state {
        Some(RunState::Running) | None => "archive_import.state_running",
        Some(RunState::Paused) => "archive_import.state_paused",
        Some(RunState::Cancelled) => "archive_import.state_cancelled",
        Some(RunState::Completed) => "archive_import.state_completed",
        Some(RunState::Errored) => "archive_import.state_errored",
    })
}

/// `YYYY-MM-DD` of an epoch-micros archive instant, as a UTC day — the Archive
/// step's date range ([`ArchiveSummaryView::first_at`] / `last_at`). UTC, not
/// the device's local day, because the Scope step's bare dates name whole UTC
/// days (`mail-export.md` § UX shape step 2's rule, which this page follows),
/// so the range the summary shows is the range the user types back.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn archive_import_utc_day(micros: u64) -> String {
    let days = (micros / 1_000_000 / 86_400) as i64;
    let (year, month, day) = fauna_core::caltime::civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Step 3's `archive-import-scope-audience-mode` option.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn archive_import_audience_mode_label(mode: AudienceMode) -> LocalizedText {
    LocalizedText::key(match mode {
        AudienceMode::Original => "archive_import.audience_original",
        AudienceMode::OnlyMe => "archive_import.audience_only_me",
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ArchiveImportAction {
    Refresh,
    SelectSource {
        kind: ArchiveSourceKind,
    },
    SetArchivePath {
        value: String,
    },
    /// Step 2: open the path through the `ArchiveOpener`, index it, show the
    /// summary.
    OpenArchive,
    Next,
    Back,
    ToggleCategory {
        token: String,
        selected: bool,
    },
    SetAudienceMode {
        mode: AudienceMode,
    },
    SetDateFrom {
        value: String,
    },
    SetDateTo {
        value: String,
    },
    /// Step 4's durable commit: create the folder, upload the raw zip, then
    /// the app spawns `run_import`.
    Start,
    Pause,
    Resume,
    Cancel,
    /// Step 6's one-tap profile prefill: write the archive's name, bio, links
    /// and picture onto the user's own profile. Never dispatched by the
    /// machine itself — the profile changes only on this tap.
    ApplyArchiveProfile,
}

#[cfg(test)]
mod vocabulary_tests {
    use super::*;

    fn resolve(text: LocalizedText) -> String {
        text.resolve(fauna_i18n::strings::lookup)
    }

    /// The picker orders are the apps' shared contract: the first entry is the
    /// default, and the order is what every app's select shows.
    #[test]
    fn picker_orders_are_pinned() {
        assert_eq!(
            SOURCE_KINDS,
            [ArchiveSourceKind::Facebook, ArchiveSourceKind::Instagram]
        );
        assert_eq!(
            AUDIENCE_MODES,
            [AudienceMode::Original, AudienceMode::OnlyMe]
        );
    }

    /// Every word the page's shared maps hand an app is a real string: a key
    /// missing from `en.yaml` would resolve to the key itself, which is the
    /// unknown-token fallback and nothing else.
    #[test]
    fn every_page_vocabulary_key_is_a_shipped_string() {
        for token in fauna_archive::Category::ALL.iter().map(|c| c.token()) {
            let key = archive_import_category_label(token).key;
            assert!(
                fauna_i18n::strings::lookup(&key).is_some(),
                "category {token}: {key}"
            );
        }
        for state in [
            None,
            Some(RunState::Running),
            Some(RunState::Paused),
            Some(RunState::Cancelled),
            Some(RunState::Completed),
            Some(RunState::Errored),
        ] {
            let key = archive_import_run_state_label(state).key;
            assert!(
                fauna_i18n::strings::lookup(&key).is_some(),
                "{state:?}: {key}"
            );
        }
        for mode in [AudienceMode::Original, AudienceMode::OnlyMe] {
            let key = archive_import_audience_mode_label(mode).key;
            assert!(
                fauna_i18n::strings::lookup(&key).is_some(),
                "{mode:?}: {key}"
            );
        }
        for kind in [ArchiveSourceKind::Facebook, ArchiveSourceKind::Instagram] {
            let key = archive_import_source_help(kind).key;
            assert!(
                fauna_i18n::strings::lookup(&key).is_some(),
                "{kind:?}: {key}"
            );
            assert_eq!(archive_import_source_kind_label(kind), kind.label());
        }
    }

    /// The archive day is the UTC day: 2021-01-07T06:13:20Z, and a second
    /// before and after UTC midnight either side of it.
    #[test]
    fn an_archive_instant_renders_as_its_utc_day() {
        const MICROS: u64 = 1_000_000;
        assert_eq!(archive_import_utc_day(1_610_000_000 * MICROS), "2021-01-07");
        assert_eq!(archive_import_utc_day(1_609_977_599 * MICROS), "2021-01-06");
        assert_eq!(archive_import_utc_day(1_610_064_000 * MICROS), "2021-01-08");
        assert_eq!(archive_import_utc_day(0), "1970-01-01");
    }

    #[test]
    fn a_category_this_build_cannot_name_shows_its_token() {
        assert_eq!(resolve(archive_import_category_label("stories")), "stories");
        assert_eq!(resolve(archive_import_category_label("posts")), "Posts");
    }

    /// `fauna-archive` depends on no `fauna-*` crate, so its platform tokens
    /// and Fauna's post `source` vocabulary (`fauna_core::source`) are two
    /// spellings of one set — pinned equal here, in the one crate that knows
    /// both: every archive platform is a source token the nest indexes and
    /// the feed badges, and every archive source token is a platform this
    /// crate can pick. A platform added on one side without the other fails
    /// here, not in a badge.
    #[test]
    fn the_archive_platforms_and_the_source_vocabulary_are_one_set() {
        let platforms: Vec<&str> = fauna_archive::Platform::ALL
            .iter()
            .map(|p| p.token())
            .collect();
        assert_eq!(platforms, fauna_core::source::ARCHIVE_PLATFORMS);
        for platform in fauna_archive::Platform::ALL {
            assert!(fauna_core::source::is_native(platform.token()));
            assert_eq!(
                fauna_core::source::normalize(platform.token()).as_deref(),
                Some(platform.token()),
                "an archive token is already in Fauna's normalized form"
            );
            let kind = ArchiveSourceKind::from_platform(&platform)
                .expect("every platform in ALL has a picker entry");
            assert_eq!(
                kind.platform(),
                platform,
                "the snapshot's picker mirrors the named set"
            );
            assert_eq!(kind.label(), platform.label());
            let url = kind.export_request_url();
            assert!(url.starts_with("https://"), "{url}");
            assert!(
                url.contains(platform.token()),
                "step 1 links to the chosen platform's own export page: {url}"
            );
        }
    }
}
