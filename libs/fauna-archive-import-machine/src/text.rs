//! The `archive-import` page's lines, composed once (`docs/goal/behavior/archive-import.md`
//! § The wizard and its machine): the source link, the archive summary, the
//! Scope rows and audience sentence, the Confirm summary, the Progress line,
//! bar and rows, the Done summary and the folder link — as [`ComposedText`],
//! so each app resolves them through its own i18n pipeline and paints them,
//! and no app writes the wording, the date and size rendering, the order or
//! the joining punctuation again.
//!
//! The byte sizes and durations are themselves translatable
//! (`fauna_core::format::byte_size` / `duration_secs`), which is what the
//! `nested` half of [`ComposedText`] carries. Nesting is one level deep, so a
//! line that used to be two templates joined with ` · ` (the Progress line's
//! counters and timing, the Done summary and its unplaced-comments clause) is
//! one template of its own.

use fauna_core::format::{byte_size, duration_secs, quota_fraction};
use fauna_core::localized::{ComposedText, LocalizedText};
use serde::{Deserialize, Serialize};

use crate::snapshot::{
    ArchiveImportSnapshot, ArchiveSummaryView, AudienceMode, CategoryRow,
    archive_import_category_label, archive_import_run_state_label, archive_import_utc_day,
};

/// A Progress-step row: the category's word (the item's name) and its
/// `imported of count` line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ArchiveImportProgressRowText {
    pub token: String,
    pub label: LocalizedText,
    pub progress: ComposedText,
}

/// Every composed line of the page for one snapshot — what an app paints
/// after each render, in one call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ArchiveImportPageText {
    /// Step 1's `archive-import-source-export-link` label.
    pub source_link: ComposedText,
    /// Step 2's `archive-import-archive-summary`; `None` before an archive
    /// is indexed.
    pub archive_summary: Option<ComposedText>,
    /// Step 3's `archive-import-scope-category-item` labels, one per
    /// [`ArchiveImportSnapshot::categories`] entry, same order.
    pub scope_rows: Vec<ComposedText>,
    /// Step 3's `archive-import-scope-audience-summary`.
    pub audience_summary: ComposedText,
    /// Step 4's `archive-import-confirm-summary`.
    pub confirm_summary: ComposedText,
    /// Step 5's `archive-import-progress-summary`: state, counters, timing.
    pub progress_summary: ComposedText,
    /// Step 5's bar, over SETTLED records (imported + skipped), `0.0..=1.0`.
    pub progress_fraction: f64,
    /// Step 5's per-category rows — the selected, importable categories only.
    pub progress_rows: Vec<ArchiveImportProgressRowText>,
    /// Step 6's `archive-import-done-summary`.
    pub done_summary: ComposedText,
    /// Step 6's `archive-import-folder-link` label.
    pub folder_link: ComposedText,
}

/// The whole page's lines for `s`.
pub fn page_text(s: &ArchiveImportSnapshot) -> ArchiveImportPageText {
    ArchiveImportPageText {
        source_link: source_link(&source_export_url(s)),
        archive_summary: s.summary.as_ref().map(archive_summary),
        scope_rows: s.categories.iter().map(scope_row).collect(),
        audience_summary: audience_summary(s.summary.as_ref(), s.audience_mode),
        confirm_summary: confirm_summary(s.confirm_records, s.confirm_bytes),
        progress_summary: progress_summary(s),
        progress_fraction: progress_fraction(s),
        progress_rows: s
            .categories
            .iter()
            .filter(|c| c.importable && c.selected)
            .map(progress_row)
            .collect(),
        done_summary: done_summary(s),
        folder_link: folder_link(s.folder_name.as_deref().unwrap_or_default()),
    }
}

/// Step 1's deep link: the snapshot's derived `source_export_url`, or the
/// chosen platform's shared constant when it is empty. Never typed by anyone,
/// so it is safe to hand to the OS opener.
pub fn source_export_url(s: &ArchiveImportSnapshot) -> String {
    if s.source_export_url.is_empty() {
        s.source_kind.export_request_url().to_string()
    } else {
        s.source_export_url.clone()
    }
}

/// Step 1's link label for `url` — a pure function of the URL, so a picker's
/// handler can paint the new platform's link before its dispatch returns.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn archive_import_source_link(url: &str) -> ComposedText {
    source_link(url)
}

fn source_link(url: &str) -> ComposedText {
    ComposedText::args("archive_import.source_link_fmt", [("url", url)])
}

/// Step 2's summary: platform, owner, the UTC-day range (or the undated
/// form when no record carries a date), the archive's and its media's sizes.
pub fn archive_summary(summary: &ArchiveSummaryView) -> ComposedText {
    let sizes = [
        ("archive_size", byte_size(summary.archive_bytes)),
        ("media_size", byte_size(summary.media_bytes)),
    ];
    if summary.first_at == 0 {
        ComposedText::with_nested(
            "archive_import.archive_summary_undated",
            [
                ("platform", summary.platform_label.as_str()),
                ("owner", summary.owner_display_name.as_str()),
            ],
            sizes,
        )
    } else {
        ComposedText::with_nested(
            "archive_import.archive_summary_fmt",
            [
                ("platform", summary.platform_label.as_str()),
                ("owner", summary.owner_display_name.as_str()),
                ("first", &archive_import_utc_day(summary.first_at)),
                ("last", &archive_import_utc_day(summary.last_at)),
            ],
            sizes,
        )
    }
}

/// Step 3's row label: the category's word and its count; a model-only
/// category (never selectable) says it is kept in the archive for later.
pub fn scope_row(row: &CategoryRow) -> ComposedText {
    let key = if row.importable {
        "archive_import.scope_category_row_fmt"
    } else {
        "archive_import.scope_category_kept_fmt"
    };
    ComposedText::with_nested(
        key,
        [("count", &row.count.to_string())],
        [("category", archive_import_category_label(&row.token))],
    )
}

/// Step 3's audience sentence for `mode`: what known vs. unknown audiences
/// become, or the Only-me override. A pure function of the summary and the
/// mode, so a picker's handler can paint it before its dispatch returns;
/// no summary yet reads as zero of each.
pub fn audience_summary(summary: Option<&ArchiveSummaryView>, mode: AudienceMode) -> ComposedText {
    match mode {
        AudienceMode::OnlyMe => ComposedText::key("archive_import.scope_audience_summary_only_me"),
        AudienceMode::Original => {
            let (known, unknown) =
                summary.map_or((0, 0), |s| (s.known_audience, s.unknown_audience));
            ComposedText::args(
                "archive_import.scope_audience_summary_fmt",
                [
                    ("known", &known.to_string()),
                    ("unknown", &unknown.to_string()),
                ],
            )
        }
    }
}

/// [`audience_summary`] over UniFFI (records cross by value).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn archive_import_audience_summary(
    summary: Option<ArchiveSummaryView>,
    mode: AudienceMode,
) -> ComposedText {
    audience_summary(summary.as_ref(), mode)
}

/// Step 4's totals: the records to import and about how much to upload.
pub fn confirm_summary(records: u64, bytes: u64) -> ComposedText {
    ComposedText::with_nested(
        "archive_import.confirm_summary_fmt",
        [("records", &records.to_string())],
        [("bytes", byte_size(bytes))],
    )
}

/// Step 5's line: the run's state, the counters, then elapsed and the
/// estimate (or that it is still estimating).
pub fn progress_summary(s: &ArchiveImportSnapshot) -> ComposedText {
    let counters = [
        ("imported", s.imported.to_string()),
        ("total", s.total.to_string()),
        ("skipped", s.skipped.to_string()),
    ];
    let pairs = counters.each_ref().map(|(n, v)| (*n, v.as_str()));
    let state = ("state", archive_import_run_state_label(s.run_state));
    let elapsed = ("elapsed", duration_secs(s.elapsed_secs));
    match s.eta_secs {
        Some(eta) => ComposedText::with_nested(
            "archive_import.progress_line_fmt",
            pairs,
            [state, elapsed, ("eta", duration_secs(eta))],
        ),
        None => ComposedText::with_nested(
            "archive_import.progress_line_estimating_fmt",
            pairs,
            [state, elapsed],
        ),
    }
}

/// Step 5's bar: the settled (imported + skipped) share of the total.
pub fn progress_fraction(s: &ArchiveImportSnapshot) -> f64 {
    quota_fraction((s.imported + s.skipped) as i64, s.total as i64)
}

/// Step 5's per-category row.
pub fn progress_row(row: &CategoryRow) -> ArchiveImportProgressRowText {
    ArchiveImportProgressRowText {
        token: row.token.clone(),
        label: archive_import_category_label(&row.token),
        progress: ComposedText::args(
            "archive_import.progress_row_fmt",
            [
                ("imported", &row.imported.to_string()),
                ("count", &row.count.to_string()),
                ("skipped", &row.skipped.to_string()),
            ],
        ),
    }
}

/// Step 6's summary: imported and skipped, naming the comments no rung could
/// place under a post when there are any (`archive-import.md` § What each
/// category becomes → *Placement under the owner's own post*), and — once
/// links exist — how many comments and reactions came back from linked
/// contacts and how many could not be matched (§ Linking and merging,
/// ruling 11). Each combination is one template: nesting is one level deep.
pub fn done_summary(s: &ArchiveImportSnapshot) -> ComposedText {
    let imported = s.imported.to_string();
    let skipped = s.skipped.to_string();
    let unplaced = s.comments_unplaced.to_string();
    let claimed = s.link_claimed.to_string();
    let unmatched = s.link_unmatched.to_string();
    let links = s.link_claimed + s.link_unmatched > 0;
    let (i, k, c) = (imported.as_str(), skipped.as_str(), unplaced.as_str());
    let (cl, um) = (claimed.as_str(), unmatched.as_str());
    match (s.comments_unplaced > 0, links) {
        (false, false) => ComposedText::args(
            "archive_import.done_summary_fmt",
            [("imported", i), ("skipped", k)],
        ),
        (true, false) => ComposedText::args(
            "archive_import.done_summary_unplaced_fmt",
            [("imported", i), ("skipped", k), ("count", c)],
        ),
        (false, true) => ComposedText::args(
            "archive_import.done_summary_links_fmt",
            [
                ("imported", i),
                ("skipped", k),
                ("claimed", cl),
                ("unmatched", um),
            ],
        ),
        (true, true) => ComposedText::args(
            "archive_import.done_summary_unplaced_links_fmt",
            [
                ("imported", i),
                ("skipped", k),
                ("count", c),
                ("claimed", cl),
                ("unmatched", um),
            ],
        ),
    }
}

/// The platform's canonical label for a `ArchiveLinkView::platform` token,
/// or the token itself for one this build cannot name.
fn platform_label(token: &str) -> String {
    fauna_archive::Platform::parse_token(token)
        .as_ref()
        .and_then(crate::snapshot::ArchiveSourceKind::from_platform)
        .map_or_else(|| token.to_string(), |k| k.label().to_string())
}

/// The Contacts row's `contact-archive-link-prompt` (ruling 8): the
/// same-person question, naming the platform and the name the viewer's own
/// archive recorded. `contact` is the row's display name, the app's own
/// resolver's answer.
pub fn link_prompt(view: &crate::link::ArchiveLinkView, contact: &str) -> ComposedText {
    let platform = platform_label(&view.platform);
    if view.archived_name.is_empty() {
        ComposedText::args(
            "archive_import.link_prompt_unnamed_fmt",
            [("contact", contact), ("platform", platform.as_str())],
        )
    } else {
        ComposedText::args(
            "archive_import.link_prompt_fmt",
            [
                ("contact", contact),
                ("archived_name", view.archived_name.as_str()),
                ("platform", platform.as_str()),
            ],
        )
    }
}

/// The Contacts row's `contact-archive-linked` (ruling 8): "Linked:
/// Facebook", plus the unmatched count once it is above zero (ruling 5:
/// the Contacts row counts unmatched items honestly).
pub fn linked_label(view: &crate::link::ArchiveLinkView) -> ComposedText {
    let platform = platform_label(&view.platform);
    if view.unmatched > 0 {
        ComposedText::args(
            "archive_import.linked_unmatched_fmt",
            [
                ("platform", platform.as_str()),
                ("unmatched", &view.unmatched.to_string()),
            ],
        )
    } else {
        ComposedText::args(
            "archive_import.linked_fmt",
            [("platform", platform.as_str())],
        )
    }
}

/// Step 6's folder link label.
pub fn folder_link(folder: &str) -> ComposedText {
    ComposedText::args("archive_import.folder_link_fmt", [("folder", folder)])
}

/// [`page_text`] over UniFFI (records cross by value).
/// [`link_prompt`] through UniFFI — `contact-archive-link-prompt`'s words.
#[cfg(feature = "uniffi")]
#[uniffi::export]
pub fn archive_link_prompt(view: crate::link::ArchiveLinkView, contact: String) -> ComposedText {
    link_prompt(&view, &contact)
}

/// [`linked_label`] through UniFFI — `contact-archive-linked`'s words.
#[cfg(feature = "uniffi")]
#[uniffi::export]
pub fn archive_linked_label(view: crate::link::ArchiveLinkView) -> ComposedText {
    linked_label(&view)
}

#[cfg(feature = "uniffi")]
#[uniffi::export]
pub fn archive_import_page_text(snapshot: ArchiveImportSnapshot) -> ArchiveImportPageText {
    page_text(&snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::RunState;

    fn resolve(text: &ComposedText) -> String {
        text.resolve(fauna_i18n::strings::lookup)
    }

    fn summary() -> ArchiveSummaryView {
        ArchiveSummaryView {
            platform_label: "Facebook".into(),
            owner_display_name: "Test Owner".into(),
            first_at: 1_580_000_000_000_000,
            last_at: 1_650_000_002_000_000,
            media_bytes: 62,
            archive_bytes: 4_096,
            known_audience: 3,
            unknown_audience: 2,
        }
    }

    fn row(token: &str, importable: bool, selected: bool) -> CategoryRow {
        CategoryRow {
            token: token.into(),
            count: 5,
            importable,
            selected,
            imported: 2,
            skipped: 1,
        }
    }

    #[test]
    fn the_archive_summary_renders_dates_sizes_and_the_undated_form() {
        assert_eq!(
            resolve(&archive_summary(&summary())),
            "Facebook · Test Owner · 2020-01-26 – 2022-04-15 · 4 KB archive, 62 B of photos and videos"
        );
        let undated = ArchiveSummaryView {
            first_at: 0,
            last_at: 0,
            ..summary()
        };
        assert_eq!(
            resolve(&archive_summary(&undated)),
            "Facebook · Test Owner · no dated records · 4 KB archive, 62 B of photos and videos"
        );
    }

    #[test]
    fn the_audience_sentence_follows_the_mode() {
        let original = resolve(&audience_summary(Some(&summary()), AudienceMode::Original));
        assert!(original.starts_with("3 posts and albums"), "{original}");
        assert!(original.contains("2 have none recorded"), "{original}");
        assert_eq!(
            resolve(&audience_summary(Some(&summary()), AudienceMode::OnlyMe)),
            "Everything will be visible only to you."
        );
        assert!(resolve(&audience_summary(None, AudienceMode::Original)).starts_with("0 posts"));
    }

    #[test]
    fn scope_rows_name_the_category_and_mark_the_kept_ones() {
        assert_eq!(resolve(&scope_row(&row("posts", true, true))), "Posts (5)");
        assert_eq!(
            resolve(&scope_row(&row("friends", false, false))),
            "Friends (5) — kept in the archive for later"
        );
    }

    #[test]
    fn the_progress_line_carries_state_counters_and_timing() {
        let mut s = ArchiveImportSnapshot::empty();
        s.run_state = Some(RunState::Running);
        s.imported = 2;
        s.total = 4;
        s.skipped = 1;
        s.elapsed_secs = 120;
        assert_eq!(
            resolve(&progress_summary(&s)),
            "Importing · 2 of 4 · 1 skipped · 2m elapsed · estimating the time left"
        );
        s.eta_secs = Some(3_660);
        s.run_state = Some(RunState::Paused);
        assert_eq!(
            resolve(&progress_summary(&s)),
            "Paused · 2 of 4 · 1 skipped · 2m elapsed · about 1h 1m left"
        );
        assert!((progress_fraction(&s) - 0.75).abs() < 1e-9);
    }

    #[test]
    fn progress_rows_are_the_selected_importable_categories() {
        let mut s = ArchiveImportSnapshot::empty();
        s.categories = vec![
            row("posts", true, true),
            row("albums", true, false),
            row("friends", false, false),
        ];
        let page = page_text(&s);
        assert_eq!(page.scope_rows.len(), 3);
        assert_eq!(page.progress_rows.len(), 1);
        let only = &page.progress_rows[0];
        assert_eq!(only.token, "posts");
        assert_eq!(only.label.resolve(fauna_i18n::strings::lookup), "Posts");
        assert_eq!(resolve(&only.progress), "2 of 5 · 1 skipped");
    }

    #[test]
    fn the_done_summary_names_unplaced_comments_only_when_there_are_some() {
        let mut s = ArchiveImportSnapshot::empty();
        s.imported = 7;
        s.skipped = 2;
        assert_eq!(resolve(&done_summary(&s)), "7 imported · 2 skipped");
        s.comments_unplaced = 3;
        assert_eq!(
            resolve(&done_summary(&s)),
            "7 imported · 2 skipped · 3 comments could not be placed under a post"
        );
    }

    #[test]
    fn the_done_summary_says_what_came_back_from_linked_contacts_once_links_exist() {
        let mut s = ArchiveImportSnapshot::empty();
        s.imported = 7;
        s.skipped = 2;
        s.link_claimed = 4;
        s.link_unmatched = 1;
        assert_eq!(
            resolve(&done_summary(&s)),
            "7 imported · 2 skipped · 4 comments and reactions came back from linked contacts · 1 could not be matched to a post"
        );
        s.comments_unplaced = 3;
        assert_eq!(
            resolve(&done_summary(&s)),
            "7 imported · 2 skipped · 3 comments could not be placed under a post · 4 comments and reactions came back from linked contacts · 1 could not be matched to a post"
        );
    }

    fn a_view(
        state: crate::link::ArchiveLinkState,
        name: &str,
        unmatched: u64,
    ) -> crate::link::ArchiveLinkView {
        crate::link::ArchiveLinkView {
            contact: "ab".repeat(32),
            state,
            platform: "facebook".into(),
            archived_name: name.into(),
            claimed: 2,
            unmatched,
        }
    }

    #[test]
    fn the_contact_rows_prompt_names_the_platform_and_the_archived_name() {
        use crate::link::ArchiveLinkState::{Asked, Linked};
        assert_eq!(
            resolve(&link_prompt(&a_view(Asked, "Friend Two", 0), "bob")),
            "Is bob the “Friend Two” from your Facebook friends?"
        );
        assert_eq!(
            resolve(&link_prompt(&a_view(Asked, "", 0), "bob")),
            "Is bob one of your Facebook friends?"
        );
        assert_eq!(
            resolve(&linked_label(&a_view(Linked, "x", 0))),
            "Linked: Facebook"
        );
        assert_eq!(
            resolve(&linked_label(&a_view(Linked, "x", 3))),
            "Linked: Facebook · 3 could not be matched to a post"
        );
    }

    #[test]
    fn the_links_and_confirm_line_read_their_templates() {
        let s = ArchiveImportSnapshot::empty();
        let page = page_text(&s);
        assert_eq!(
            resolve(&page.source_link),
            format!(
                "Request your export here: {}",
                s.source_kind.export_request_url()
            )
        );
        assert_eq!(
            resolve(&folder_link("Facebook archive")),
            "Archive folder: Facebook archive"
        );
        assert_eq!(
            resolve(&confirm_summary(12, 2_048)),
            "12 records · about 2 KB to upload"
        );
    }

    /// Every key a composer emits is a shipped string — a missing one would
    /// resolve to the key itself on every app.
    #[test]
    fn every_composed_key_is_a_shipped_string() {
        let mut s = ArchiveImportSnapshot::empty();
        s.summary = Some(summary());
        s.categories = vec![row("posts", true, true), row("friends", false, false)];
        s.comments_unplaced = 1;
        let mut texts = vec![];
        {
            use crate::link::ArchiveLinkState::{Asked, Linked};
            let mut l = s.clone();
            l.link_claimed = 1;
            texts.push(done_summary(&l));
            l.comments_unplaced = 0;
            texts.push(done_summary(&l));
            texts.extend([
                link_prompt(&a_view(Asked, "n", 0), "c"),
                link_prompt(&a_view(Asked, "", 0), "c"),
                linked_label(&a_view(Linked, "n", 0)),
                linked_label(&a_view(Linked, "n", 1)),
            ]);
        }
        for eta in [None, Some(5)] {
            s.eta_secs = eta;
            let p = page_text(&s);
            texts.extend([
                p.source_link,
                p.archive_summary.unwrap(),
                p.audience_summary,
                p.confirm_summary,
                p.progress_summary,
                p.done_summary,
                p.folder_link,
            ]);
            texts.extend(p.scope_rows);
            texts.extend(p.progress_rows.into_iter().map(|r| r.progress));
        }
        texts.push(audience_summary(None, AudienceMode::OnlyMe));
        for t in &texts {
            for key in std::iter::once(&t.text.key).chain(t.nested.values().map(|n| &n.key)) {
                assert!(fauna_i18n::strings::lookup(key).is_some(), "{key}");
            }
        }
    }
}
