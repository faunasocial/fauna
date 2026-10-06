//! What the per-app `archive-import` page renders and the actions it
//! dispatches — `archive-import.md` § The wizard and its machine, steps 1–6.
//! Element IDs are the proposed `archive-import-*` set (rule-A approval
//! pending; slice 4 lands them in ui.yaml).
//!
//! Every type here is FFI-flat — plain scalars, `String`, `Option` and `Vec`
//! of the same — so one snapshot crosses UniFFI (native) and wasm-bindgen
//! (web) unchanged and all 7 apps render from the identical shape
//! (priorities #1–#3).

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ArchiveImportSnapshot {
    pub step: ArchiveImportStep,
    pub source_kind: ArchiveSourceKind,
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
}

impl ArchiveImportSnapshot {
    /// The page as it paints before `hydrate` has answered: step 1, nothing
    /// known about the nest or any archive.
    pub fn empty() -> Self {
        Self {
            step: ArchiveImportStep::Source,
            source_kind: ArchiveSourceKind::Facebook,
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
            skip_log: Vec::new(),
            nest_supports_hidden_tiers: None,
            resume_available: false,
            status: ArchiveImportStatus::Idle,
            error: None,
        }
    }
}

impl Default for ArchiveImportSnapshot {
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(test)]
mod vocabulary_tests {
    use super::ArchiveSourceKind;

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
        }
    }
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
}
