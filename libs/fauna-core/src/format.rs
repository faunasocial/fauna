use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::data::{FeedSources, UnknownPeerDm, UnknownSenderMail};
use crate::localized::LocalizedText;
use crate::obligation::{ContentFloor, ContentPolicy, GUARDIAN_FLOOR_CATEGORIES};

/// Result of a 3-way merge attempt.
pub enum MergeResult {
    Merged(Vec<u8>),
    Conflicts {
        merged: Vec<u8>,
        conflict_count: usize,
    },
}

/// Per-set conflict policy (file-sync.md § Conflicts, ratified 2026-07-10).
///
/// Governs how a detected sync conflict auto-resolves. Both arms are risk-free by
/// construction — the losing version is always retained in version history — so an
/// unknown future wire value degrades to [`ConflictPolicy::Auto`] (the default).
/// Wire/DB representation is the snake_case string (`"auto"` / `"latest_wins_always"`);
/// the `folders.conflict_policy` column and the `FolderSummary` wire field carry it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    /// Text-like files attempt a clean three-way merge; everything else (and any
    /// overlapping-hunk merge) falls to latest-writer-wins.
    #[default]
    Auto,
    /// Skip the merge attempt entirely; every conflict resolves latest-writer-wins.
    LatestWinsAlways,
}

impl ConflictPolicy {
    /// The canonical wire/DB string for this policy.
    pub fn as_str(self) -> &'static str {
        match self {
            ConflictPolicy::Auto => "auto",
            ConflictPolicy::LatestWinsAlways => "latest_wins_always",
        }
    }

    /// Parse a wire/DB string. Unknown values degrade to [`ConflictPolicy::Auto`]
    /// (both arms retain the loser, so degrading is safe; `Auto` is the ratified
    /// default a fresh row carries).
    pub fn from_wire(s: &str) -> Self {
        match s {
            "latest_wins_always" => ConflictPolicy::LatestWinsAlways,
            _ => ConflictPolicy::Auto,
        }
    }
}

/// A semantic chunk with a stable identity across versions.
pub struct SemanticChunk {
    pub id: String,
    pub data: Vec<u8>,
}

/// Pluggable format adapter for intelligent chunking and merging.
pub trait FormatAdapter: Send + Sync {
    fn extensions(&self) -> &[&str];
    fn semantic_chunks(&self, content: &[u8]) -> Result<Option<Vec<SemanticChunk>>>;
    fn can_merge(&self, base: &[u8], ours: &[u8], theirs: &[u8]) -> bool;
    fn merge(&self, base: &[u8], ours: &[u8], theirs: &[u8]) -> Result<MergeResult>;
}

/// L0: Opaque blob adapter. No merging, no semantic chunking.
pub struct OpaqueAdapter;

impl FormatAdapter for OpaqueAdapter {
    fn extensions(&self) -> &[&str] {
        &[]
    }

    fn semantic_chunks(&self, _content: &[u8]) -> Result<Option<Vec<SemanticChunk>>> {
        Ok(None)
    }

    fn can_merge(&self, _base: &[u8], _ours: &[u8], _theirs: &[u8]) -> bool {
        false
    }

    fn merge(&self, _base: &[u8], _ours: &[u8], _theirs: &[u8]) -> Result<MergeResult> {
        bail!("opaque adapter does not support merging")
    }
}

/// Registry of format adapters. Looks up by file extension, falls back to L0.
pub struct FormatRegistry {
    adapters: Vec<Box<dyn FormatAdapter>>,
    fallback: OpaqueAdapter,
}

impl FormatRegistry {
    /// Create an empty registry with the opaque fallback.
    pub fn new() -> Self {
        Self {
            adapters: Vec::new(),
            fallback: OpaqueAdapter,
        }
    }

    /// Register a format adapter.
    pub fn register(&mut self, adapter: Box<dyn FormatAdapter>) {
        self.adapters.push(adapter);
    }

    /// Look up an adapter by file path extension. Falls back to [`OpaqueAdapter`].
    pub fn adapter_for(&self, path: &str) -> &dyn FormatAdapter {
        let ext = path.rsplit('.').next().unwrap_or("");
        for adapter in &self.adapters {
            for supported in adapter.extensions() {
                // Compare without leading dot — extensions() returns e.g. "md", not ".md"
                let supported_bare = supported.strip_prefix('.').unwrap_or(supported);
                if ext.eq_ignore_ascii_case(supported_bare) {
                    return adapter.as_ref();
                }
            }
        }
        &self.fallback
    }
}

impl Default for FormatRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod reach_policy_format_tests {
    use super::*;

    /// The catalogs are the ratified ones (`family-safety.md` § Guardian policy —
    /// `allow / hold / reject` and `allow / block`), default first, and each
    /// option's value is the canonical wire spelling.
    #[test]
    fn option_catalogs_are_the_ratified_values_in_order() {
        let unknown: Vec<String> = unknown_sender_options()
            .into_iter()
            .map(|o| o.value)
            .collect();
        assert_eq!(unknown, ["allow", "hold", "reject"]);
        let feed: Vec<String> = feed_sources_options()
            .into_iter()
            .map(|o| o.value)
            .collect();
        assert_eq!(feed, ["allow", "block"]);

        // Each option carries its own knob's label key — the two catalogs never
        // share a value→label map.
        assert_eq!(unknown_sender_options()[1].label.key, "family.value_hold");
        assert_eq!(feed_sources_options()[1].label.key, "family.value_block");
    }

    /// `family-safety.md` § Implementation status — *"an unrecognized value renders
    /// as the strictest option (`hold` / `block`), never the permissive one"*. The
    /// two knobs fail closed to **different** options; this is the pin that a
    /// single shared value→label map would break.
    #[test]
    fn an_unparseable_value_fails_closed_per_knob_never_to_allow() {
        for junk in ["", "hologram", "ALLOW", "Allow", "allow-ish", "block", "0"] {
            assert_eq!(
                unknown_sender_label(junk).key,
                "family.value_hold",
                "unknown_sender_mail {junk:?} must fail closed to hold"
            );
        }
        for junk in ["", "hologram", "ALLOW", "Allow", "reject", "hold", "1"] {
            assert_eq!(
                feed_sources_label(junk).key,
                "family.value_block",
                "feed_sources {junk:?} must fail closed to block"
            );
        }
        // Only the exact wire spelling reaches the permissive label.
        assert_eq!(unknown_sender_label("allow").key, "family.value_allow");
        assert_eq!(feed_sources_label("allow").key, "family.value_allow");
    }

    /// The windows merged-`ValueLabel` trap, pinned: `"reject"` is a value of the
    /// *other* knob, so `feed_sources` must never render "Reject" — it is not in
    /// this knob's catalog.
    #[test]
    fn a_feed_sources_value_never_renders_the_other_knobs_label() {
        assert_eq!(feed_sources_label("reject").key, "family.value_block");
        assert_eq!(feed_sources_label("hold").key, "family.value_block");
        // ...and the converse: `block` is not an unknown_sender_mail value.
        assert_eq!(unknown_sender_label("block").key, "family.value_hold");
    }

    /// Round-trip: every catalog value labels as itself, so a select built from
    /// the catalog and read back cannot drift.
    #[test]
    fn every_catalog_value_round_trips_through_its_label() {
        for opt in unknown_sender_options() {
            assert_eq!(unknown_sender_label(&opt.value), opt.label);
        }
        for opt in feed_sources_options() {
            assert_eq!(feed_sources_label(&opt.value), opt.label);
        }
    }

    /// `FAIL_CLOSED` is the option a picker degrades to, and it must stay exactly
    /// what [`UnknownSenderMail::from_wire`] / [`FeedSources::from_wire`] give an
    /// unparseable value — two statements of one safety rule, pinned together so
    /// they cannot drift.
    #[test]
    fn the_fail_closed_option_matches_the_wire_parse_and_is_never_permissive() {
        assert_eq!(
            UnknownSenderMail::FAIL_CLOSED,
            UnknownSenderMail::from_wire("hologram")
        );
        assert_eq!(FeedSources::FAIL_CLOSED, FeedSources::from_wire("hologram"));
        // Never the permissive option — which is index 0 of each catalog.
        assert_ne!(UnknownSenderMail::FAIL_CLOSED, UnknownSenderMail::Allow);
        assert_ne!(FeedSources::FAIL_CLOSED, FeedSources::Allow);
        assert_eq!(UnknownSenderMail::ORDER[0], UnknownSenderMail::Allow);
        assert_eq!(FeedSources::ORDER[0], FeedSources::Allow);
    }

    /// Five lines, in the ratified knob-table order, with the bools rendered as
    /// enable/disable and the string knobs already fail-closed.
    #[test]
    fn summary_renders_five_lines_in_the_ratified_order() {
        let lines = reach_policy_summary(true, "reject", false, "block", Some("hold"));
        assert_eq!(lines.len(), 5);
        let labels: Vec<&str> = lines.iter().map(|l| l.label.key.as_str()).collect();
        assert_eq!(
            labels,
            [
                "family.policy_contact_approval_label",
                "family.policy_unknown_sender_label",
                "family.policy_federation_label",
                "family.policy_feed_sources_label",
                "family.policy_unknown_peer_dm_label",
            ]
        );
        let values: Vec<&str> = lines.iter().map(|l| l.value.key.as_str()).collect();
        assert_eq!(
            values,
            [
                "common.enable",
                "family.value_reject",
                "common.disable",
                "family.value_block",
                "family.value_hold",
            ]
        );
    }

    /// The summary is the *supervised* side's read-only view of the policy that is
    /// actually enforced — so an unparseable knob must not show it as weaker than
    /// it is. (Linux's `value_at` fell back to `values[0]` = `"allow"`; this pin is
    /// what makes that shape unrepresentable.)
    #[test]
    fn summary_fails_closed_on_unparseable_knobs() {
        let lines = reach_policy_summary(false, "hologram", true, "hologram", Some("hologram"));
        assert_eq!(lines[1].value.key, "family.value_hold");
        assert_eq!(lines[3].value.key, "family.value_block");
        assert_eq!(lines[4].value.key, "family.value_hold");
    }

    /// An **absent** `unknown_peer_dm` is the knob at its `allow` default — an
    /// untouched policy (the nest omits it at `allow`). Deliberately *not* the
    /// unparseable case: an omitted knob genuinely gates nothing, so
    /// `allow` states what is actually enforced. Failing closed here would show
    /// the guardian a restriction nobody applied.
    #[test]
    fn an_absent_unknown_peer_dm_renders_its_allow_default_not_the_fail_closed_value() {
        let lines = reach_policy_summary(false, "allow", true, "allow", None);
        assert_eq!(lines[4].value.key, "family.value_allow");
    }

    /// `contact` and `feed_source` render `summary`; the two envelope-class
    /// kinds (`mail_hold`, `dm_hold`) render `peer_address` and a
    /// `contact_request` its `peer_handle` — never `summary`, which is always
    /// empty for all three (a subject line or preview is content; an ask names
    /// *who*, never *why*).
    #[test]
    fn non_mail_hold_kinds_render_summary_never_peer_address() {
        assert_eq!(
            approval_display_text("contact", "irrelevant@example.com", "", "a knock summary"),
            Some("a knock summary")
        );
        assert_eq!(
            approval_display_text("feed_source", "", "", "petname"),
            Some("petname")
        );
    }

    /// A `mail_hold` with a real envelope address renders it, not `summary`
    /// (deliberately empty for a hold — the bug windows shipped once).
    #[test]
    fn mail_hold_renders_peer_address_not_summary() {
        assert_eq!(
            approval_display_text("mail_hold", "stranger@example.com", "", ""),
            Some("stranger@example.com")
        );
    }

    /// The SMTP null reverse-path (`MAIL FROM:<>`) leaves `peer_address` truly
    /// empty; rendering it verbatim would leave a blank row with live
    /// Approve/Deny buttons, so this returns `None` — the caller renders its own
    /// localized no-sender placeholder. Generalized to every kind's own field
    /// (see `approval_display_text_tests`).
    #[test]
    fn a_null_path_mail_hold_returns_none_for_the_caller_to_localize() {
        assert_eq!(approval_display_text("mail_hold", "", "", ""), None);
    }

    // --- content-floor catalog (family-safety.md § Content policy, Slice C) ---

    /// The content-floor picker catalog is the ratified `inherit / collapse /
    /// block`, default first, each option's value the canonical wire spelling.
    /// (`Unknown` is a deserialize-only catch-all, never a picker option.)
    #[test]
    fn content_floor_options_are_the_ratified_values_in_order() {
        let values: Vec<String> = content_floor_options()
            .into_iter()
            .map(|o| o.value)
            .collect();
        assert_eq!(values, ["inherit", "collapse", "block"]);
        assert_eq!(content_floor_options()[0].label.key, "family.value_inherit");
        assert_eq!(
            content_floor_options()[1].label.key,
            "family.value_collapse"
        );
        assert_eq!(content_floor_options()[2].label.key, "family.value_block");
    }

    /// `family-safety.md` § Content policy — *"A rule value a client cannot parse
    /// renders fail-closed (`block`)"*. The same safety rule as the reach knobs,
    /// this knob's strict option is `block`.
    #[test]
    fn a_content_floor_value_a_client_cannot_parse_fails_closed_to_block() {
        for junk in ["", "hologram", "INHERIT", "Collapse", "quarantine", "9"] {
            assert_eq!(
                content_floor_label(junk).key,
                "family.value_block",
                "content floor {junk:?} must fail closed to block"
            );
        }
        // Only the exact wire spellings reach the non-strict labels.
        assert_eq!(content_floor_label("inherit").key, "family.value_inherit");
        assert_eq!(content_floor_label("collapse").key, "family.value_collapse");
        assert_eq!(content_floor_label("block").key, "family.value_block");
    }

    /// Round-trip: every catalog value labels as itself.
    #[test]
    fn every_content_floor_value_round_trips_through_its_label() {
        for opt in content_floor_options() {
            assert_eq!(content_floor_label(&opt.value), opt.label);
        }
    }

    /// `FAIL_CLOSED` matches the wire-parse degrade and is never the permissive
    /// `inherit` (index 0 of the catalog).
    #[test]
    fn the_content_floor_fail_closed_matches_the_wire_parse_and_is_never_inherit() {
        use crate::obligation::ContentFloor;
        assert_eq!(ContentFloor::FAIL_CLOSED, ContentFloor::Block);
        assert_eq!(ContentFloor::from_wire("hologram"), ContentFloor::Unknown);
        assert_ne!(ContentFloor::FAIL_CLOSED, ContentFloor::Inherit);
        assert_eq!(ContentFloor::ORDER[0], ContentFloor::Inherit);
    }

    /// The content-policy summary shows **only the non-inherit floors** (an
    /// `inherit` category adds nothing over the ward's own preferences, so it is
    /// not a rule to render), in the canonical category order, with the value
    /// fail-closed.
    #[test]
    fn content_policy_summary_renders_only_non_inherit_floors_in_order() {
        use crate::obligation::{ContentFloor, ContentPolicy};
        let policy = ContentPolicy {
            nsfw: ContentFloor::Block,
            spam: ContentFloor::Inherit,
            phishing: ContentFloor::Collapse,
            commercial: ContentFloor::Inherit,
        };
        let lines = content_policy_summary(&policy);
        let labels: Vec<&str> = lines.iter().map(|l| l.label.key.as_str()).collect();
        assert_eq!(
            labels,
            [
                "family.policy_content_nsfw_label",
                "family.policy_content_phishing_label",
            ]
        );
        let values: Vec<&str> = lines.iter().map(|l| l.value.key.as_str()).collect();
        assert_eq!(values, ["family.value_block", "family.value_collapse"]);

        // An all-inherit policy (the default) renders no content lines.
        assert!(content_policy_summary(&ContentPolicy::default()).is_empty());
    }

    #[test]
    fn content_notice_line_pairs_category_label_with_count() {
        // The guardian readout line reuses the same category-label map as the
        // editor/summary and carries only a count — never content.
        let line = content_notice_line("spam", 3);
        assert_eq!(line.label.key, "family.policy_content_spam_label");
        assert_eq!(line.value.key, "family.ward_content_notice_count");
        assert_eq!(line.value.args.get("count").map(String::as_str), Some("3"));
        // Every canonical category resolves to its own label key (no drift).
        for (cat, key) in [
            ("nsfw", "family.policy_content_nsfw_label"),
            ("phishing", "family.policy_content_phishing_label"),
            ("commercial", "family.policy_content_commercial_label"),
        ] {
            assert_eq!(content_notice_line(cat, 1).label.key, key);
        }
    }

    #[test]
    fn usage_today_line_names_the_budget_when_there_is_one() {
        let line = usage_today_line(45, Some(120));
        assert_eq!(line.label.key, "family.ward_usage_today_label");
        assert_eq!(line.value.key, "family.ward_usage_today_of_budget");
        assert_eq!(line.value.args.get("used").map(String::as_str), Some("45"));
        assert_eq!(
            line.value.args.get("budget").map(String::as_str),
            Some("120")
        );
    }

    #[test]
    fn usage_today_line_degrades_to_the_bare_figure_with_no_budget() {
        // No denominator to invent — the readout still shows the number, which
        // is what the ward's own transparency view falls back to.
        let line = usage_today_line(45, None);
        assert_eq!(line.value.key, "family.ward_usage_today");
        assert_eq!(
            line.value.args.get("minutes").map(String::as_str),
            Some("45")
        );
    }

    #[test]
    fn both_surfaces_render_the_identical_line() {
        // The goal doc's transparency promise is that the guardian and the ward
        // see the SAME number. One shared constructor is how that holds: this
        // asserts there is no second formatting path to drift from.
        let guardian_side = usage_today_line(90, Some(120));
        let ward_side = usage_today_line(90, Some(120));
        assert_eq!(guardian_side.label.key, ward_side.label.key);
        assert_eq!(guardian_side.value.key, ward_side.value.key);
        assert_eq!(guardian_side.value.args, ward_side.value.args);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_policy_default_is_auto_and_wire_round_trips() {
        assert_eq!(ConflictPolicy::default(), ConflictPolicy::Auto);
        for p in [ConflictPolicy::Auto, ConflictPolicy::LatestWinsAlways] {
            assert_eq!(ConflictPolicy::from_wire(p.as_str()), p);
            // serde string form matches as_str (one canonical wire spelling).
            let json = serde_json::to_string(&p).unwrap();
            assert_eq!(json, format!("\"{}\"", p.as_str()));
            assert_eq!(serde_json::from_str::<ConflictPolicy>(&json).unwrap(), p);
        }
        // Unknown future value degrades to Auto, never errors.
        assert_eq!(ConflictPolicy::from_wire("hologram"), ConflictPolicy::Auto);
    }

    #[test]
    fn opaque_adapter_cannot_merge() {
        let adapter = OpaqueAdapter;
        assert!(!adapter.can_merge(b"base", b"ours", b"theirs"));
        assert!(adapter.merge(b"base", b"ours", b"theirs").is_err());
        assert!(adapter.semantic_chunks(b"hello").unwrap().is_none());
    }

    #[test]
    fn registry_falls_back_to_opaque() {
        let registry = FormatRegistry::new();
        let adapter = registry.adapter_for("photo.jpg");
        // The fallback OpaqueAdapter returns false for can_merge and None for semantic_chunks.
        assert!(!adapter.can_merge(b"", b"", b""));
        assert!(adapter.semantic_chunks(b"data").unwrap().is_none());
    }

    /// A trivial test adapter that claims ".md" and ".txt".
    struct TestMarkdownAdapter;

    impl FormatAdapter for TestMarkdownAdapter {
        fn extensions(&self) -> &[&str] {
            &["md", "txt"]
        }

        fn semantic_chunks(&self, _content: &[u8]) -> Result<Option<Vec<SemanticChunk>>> {
            Ok(Some(vec![SemanticChunk {
                id: "heading".into(),
                data: b"# Title".to_vec(),
            }]))
        }

        fn can_merge(&self, _base: &[u8], _ours: &[u8], _theirs: &[u8]) -> bool {
            true
        }

        fn merge(&self, _base: &[u8], _ours: &[u8], _theirs: &[u8]) -> Result<MergeResult> {
            Ok(MergeResult::Merged(b"merged".to_vec()))
        }
    }

    #[test]
    fn registry_matches_by_extension() {
        let mut registry = FormatRegistry::new();
        registry.register(Box::new(TestMarkdownAdapter));

        // .md should match the test adapter
        let md_adapter = registry.adapter_for("notes.md");
        assert!(md_adapter.can_merge(b"", b"", b""));
        let chunks = md_adapter.semantic_chunks(b"content").unwrap();
        assert!(chunks.is_some());
        assert_eq!(chunks.unwrap().len(), 1);

        // .txt should also match
        let txt_adapter = registry.adapter_for("readme.txt");
        assert!(txt_adapter.can_merge(b"", b"", b""));

        // .jpg should fall back to opaque
        let jpg_adapter = registry.adapter_for("photo.jpg");
        assert!(!jpg_adapter.can_merge(b"", b"", b""));
        assert!(jpg_adapter.semantic_chunks(b"data").unwrap().is_none());
    }
}

// ── Value formatting (relative time, byte sizes) ──────────────────────────────
//
// Shared display-value formatting per `docs/goal/behavior/value-formatting.md`.
// The *decision* (which time bucket, which 1024-unit) is computed once here and
// returned as an i18n key + args via `LocalizedText`, so no English string is
// baked into shared Rust — each app renders the key through its own
// localization pipeline (priority #2 logic; #3 reuses the LocalizedText carrier).

const MINUTE_MS: i64 = 60 * 1_000;
const HOUR_MS: i64 = 60 * MINUTE_MS;
const DAY_MS: i64 = 24 * HOUR_MS;
const WEEK_MS: i64 = 7 * DAY_MS;

/// Uniform relative-time buckets. Thresholds are fixed by
/// `docs/goal/behavior/value-formatting.md` § Relative time (the authority).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum RelativeTimestamp {
    JustNow,
    MinutesAgo {
        n: u32,
    },
    HoursAgo {
        n: u32,
    },
    DaysAgo {
        n: u32,
    },
    /// `≥ 7 d` old — the client renders a real date with its native, locale-aware
    /// date formatter (no shared i18n key fits a localized absolute date).
    Absolute {
        epoch_ms: i64,
    },
}

impl RelativeTimestamp {
    /// i18n key + `{count}` arg for the four relative buckets; `None` for
    /// [`RelativeTimestamp::Absolute`] (the signal: format an absolute date
    /// client-side).
    pub fn to_localized(&self) -> Option<LocalizedText> {
        match self {
            RelativeTimestamp::JustNow => Some(LocalizedText::key("time.just_now")),
            RelativeTimestamp::MinutesAgo { n } => Some(LocalizedText::key_arg(
                "time.minutes_ago",
                "count",
                n.to_string(),
            )),
            RelativeTimestamp::HoursAgo { n } => Some(LocalizedText::key_arg(
                "time.hours_ago",
                "count",
                n.to_string(),
            )),
            RelativeTimestamp::DaysAgo { n } => Some(LocalizedText::key_arg(
                "time.days_ago",
                "count",
                n.to_string(),
            )),
            RelativeTimestamp::Absolute { .. } => None,
        }
    }
}

/// Bucket `then_ms` relative to `now_ms` (both epoch milliseconds). A future
/// `then` (`> now`) clamps to [`RelativeTimestamp::JustNow`].
pub fn relative_time(now_ms: i64, then_ms: i64) -> RelativeTimestamp {
    let diff = now_ms - then_ms;
    if diff < MINUTE_MS {
        RelativeTimestamp::JustNow
    } else if diff < HOUR_MS {
        RelativeTimestamp::MinutesAgo {
            n: (diff / MINUTE_MS) as u32,
        }
    } else if diff < DAY_MS {
        RelativeTimestamp::HoursAgo {
            n: (diff / HOUR_MS) as u32,
        }
    } else if diff < WEEK_MS {
        RelativeTimestamp::DaysAgo {
            n: (diff / DAY_MS) as u32,
        }
    } else {
        RelativeTimestamp::Absolute { epoch_ms: then_ms }
    }
}

/// Boundary-friendly flattening of [`relative_time`] for the FFI / wasm clients,
/// so they never re-implement the bucket→key selection: `localized` is `Some`
/// (render the key) for the four relative buckets; when it is `None`, render
/// `absolute_epoch_ms` as a real date with the platform's native, locale-aware
/// date formatter. (Linux can use [`relative_time`] + [`RelativeTimestamp::to_localized`]
/// directly instead.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RelativeTimeDisplay {
    pub localized: Option<LocalizedText>,
    pub absolute_epoch_ms: Option<i64>,
}

/// [`relative_time`] flattened to [`RelativeTimeDisplay`] for the FFI / wasm
/// boundary (the i18n key is selected Rust-side; clients never duplicate it).
pub fn relative_time_display(now_ms: i64, then_ms: i64) -> RelativeTimeDisplay {
    match relative_time(now_ms, then_ms) {
        RelativeTimestamp::Absolute { epoch_ms } => RelativeTimeDisplay {
            localized: None,
            absolute_epoch_ms: Some(epoch_ms),
        },
        bucket => RelativeTimeDisplay {
            localized: bucket.to_localized(),
            absolute_epoch_ms: None,
        },
    }
}

/// Resolve a [`RelativeTimeDisplay`] all the way to finished text, against the
/// caller's own i18n lookup.
///
/// The flattened display above stops one step short on purpose — it is the
/// shape the FFI / wasm boundary can carry, and web deliberately renders its
/// `absolute_epoch_ms` arm with the browser's locale-aware formatter
/// (`$lib/value-format.ts`). The two **native Rust** apps have no such
/// divergence: both render the `≥ 7 d` arm as the shared plain
/// [`format_unix_local_date_ms`] date, and both resolve the bucket arm through
/// their own `strings::lookup`. That made the last step byte-identical in
/// `fauna-linux`'s `i18n` and `fauna-tui`'s `format`, so it lives here now and
/// only the lookup stays per-app — the same parameterisation
/// [`resolve_option_labels`] already uses.
///
/// Native-only (`local-clock`): the absolute arm needs the OS timezone
/// database. A wasm caller wants [`relative_time_display`] and its own date
/// formatter, which is why that door is ungated and this one is not.
///
/// A display carrying neither arm renders empty — it cannot occur through
/// [`relative_time_display`], and inventing a time would be worse than saying
/// nothing.
#[cfg(feature = "local-clock")]
pub fn relative_time_display_text<F, S>(display: &RelativeTimeDisplay, lookup: F) -> String
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    match (&display.localized, display.absolute_epoch_ms) {
        (Some(text), _) => text.resolve(lookup),
        (None, Some(epoch_ms)) => format_unix_local_date_ms(epoch_ms),
        (None, None) => String::new(),
    }
}

/// [`relative_time`] resolved to finished text in one call — the whole
/// bucket-or-local-date decision for a native Rust app.
///
/// Argument order follows [`relative_time`] (`now` before `then`), not the
/// `then, now` order `fauna-linux`'s twin happened to take; both args are epoch
/// **milliseconds**. A caller holding micros divides by 1_000 at the call site
/// and guards its own "unset" sentinel — `0` is a legitimate instant here, and
/// only the caller knows whether its field uses it as "never".
#[cfg(feature = "local-clock")]
pub fn relative_time_text<F, S>(now_ms: i64, then_ms: i64, lookup: F) -> String
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    relative_time_display_text(&relative_time_display(now_ms, then_ms), lookup)
}

/// Weekday i18n keys, Monday-first (`index` 0 = Monday … 6 = Sunday).
const WEEKDAY_KEYS: [&str; 7] = [
    "time.weekday_mon",
    "time.weekday_tue",
    "time.weekday_wed",
    "time.weekday_thu",
    "time.weekday_fri",
    "time.weekday_sat",
    "time.weekday_sun",
];

/// Contextual display bucket for a conversation/thread last-activity time,
/// computed in the *caller's local timezone* (`utc_offset_seconds`, e.g. `+7200`
/// for UTC+2). Calendar-based, not duration-based: a message at 23:50 yesterday
/// is [`Yesterday`](ConversationTimestamp::Yesterday) even though it is < 24 h
/// old. Thresholds are fixed by `docs/goal/behavior/value-formatting.md`
/// § Conversation timestamp (the authority).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ConversationTimestamp {
    /// Same local calendar day (and any future time): local wall-clock.
    Today { hour: u8, minute: u8 },
    /// The previous local calendar day.
    Yesterday,
    /// 2–6 local days ago: the local weekday of `then` (`index` 0 = Monday).
    Weekday { index: u8 },
    /// `≥ 7` local days ago — the client renders a real date with its native,
    /// locale-aware date formatter (no shared i18n key fits a localized date).
    Older { epoch_ms: i64 },
}

impl ConversationTimestamp {
    /// i18n key for the `Yesterday` / `Weekday` buckets; `None` for `Today`
    /// (render the clock) and `Older` (format an absolute date client-side).
    pub fn to_localized(&self) -> Option<LocalizedText> {
        match self {
            ConversationTimestamp::Yesterday => Some(LocalizedText::key("time.yesterday")),
            ConversationTimestamp::Weekday { index } => {
                Some(LocalizedText::key(WEEKDAY_KEYS[(*index as usize) % 7]))
            }
            _ => None,
        }
    }
}

/// Bucket `then_ms` relative to `now_ms` (both epoch ms) in the caller's local
/// timezone (`utc_offset_seconds`). See [`ConversationTimestamp`].
pub fn conversation_timestamp(
    now_ms: i64,
    then_ms: i64,
    utc_offset_seconds: i32,
) -> ConversationTimestamp {
    let offset_ms = utc_offset_seconds as i64 * 1_000;
    let local_then = then_ms + offset_ms;
    let then_day = local_then.div_euclid(DAY_MS);
    let now_day = (now_ms + offset_ms).div_euclid(DAY_MS);
    let delta_days = now_day - then_day;
    if delta_days <= 0 {
        // Same local day (or future) → local wall-clock.
        let tod = local_then.rem_euclid(DAY_MS);
        ConversationTimestamp::Today {
            hour: (tod / HOUR_MS) as u8,
            minute: ((tod % HOUR_MS) / MINUTE_MS) as u8,
        }
    } else if delta_days == 1 {
        ConversationTimestamp::Yesterday
    } else if delta_days < 7 {
        // Weekday of `then`: epoch day 0 (1970-01-01) is a Thursday (index 3,
        // Monday-first), so index = (then_day + 3) mod 7.
        ConversationTimestamp::Weekday {
            index: (then_day + 3).rem_euclid(7) as u8,
        }
    } else {
        ConversationTimestamp::Older { epoch_ms: then_ms }
    }
}

/// Boundary-friendly flattening of [`conversation_timestamp`] for the FFI / wasm
/// clients: exactly one field is `Some`. `clock` (`"HH:MM"`, 24 h, local) for
/// today; `localized` for Yesterday / a weekday name; `absolute_epoch_ms` for
/// older items (the client formats a locale-aware date, as with
/// [`RelativeTimeDisplay`]). Native apps can use the
/// [`ConversationTimestamp`] enum directly (e.g. a locale-aware 12 h clock off
/// `Today { hour, minute }`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ConversationTimestampDisplay {
    pub clock: Option<String>,
    pub localized: Option<LocalizedText>,
    pub absolute_epoch_ms: Option<i64>,
}

/// [`conversation_timestamp`] flattened to [`ConversationTimestampDisplay`].
pub fn conversation_timestamp_display(
    now_ms: i64,
    then_ms: i64,
    utc_offset_seconds: i32,
) -> ConversationTimestampDisplay {
    match conversation_timestamp(now_ms, then_ms, utc_offset_seconds) {
        ConversationTimestamp::Today { hour, minute } => ConversationTimestampDisplay {
            clock: Some(format!("{hour:02}:{minute:02}")),
            localized: None,
            absolute_epoch_ms: None,
        },
        ConversationTimestamp::Older { epoch_ms } => ConversationTimestampDisplay {
            clock: None,
            localized: None,
            absolute_epoch_ms: Some(epoch_ms),
        },
        bucket => ConversationTimestampDisplay {
            clock: None,
            localized: bucket.to_localized(),
            absolute_epoch_ms: None,
        },
    }
}

/// A fixed, non-localized `"YYYY-MM-DD HH:MM"` local wall-clock rendering of a
/// unix-seconds timestamp — for audit/technical displays (credential
/// created-at, nest-trust grant history) that intentionally do *not*
/// localize, unlike [`relative_time_display`] / [`conversation_timestamp_display`]
/// (which hand locale-aware formatting to the client). Falls back to the raw
/// number when the local offset is ambiguous (a DST fall-back hour) or the
/// value is out of range — never invents a time. Native-only (`local-clock`
/// feature): needs the OS timezone database, unavailable on wasm32 without a
/// JS-interop bridge. See `docs/goal/behavior/value-formatting.md` §
/// Absolute local timestamp display.
#[cfg(feature = "local-clock")]
pub fn format_unix_local(secs: i64) -> String {
    use chrono::{Local, TimeZone as _};
    match Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        _ => secs.to_string(),
    }
}

/// Epoch-**millisecond** input adapter over [`format_unix_local`] — the full
/// datetime twin of [`format_unix_local_date_ms`], for callers whose timestamp
/// is already in ms (most FFI timestamps are). Same rationale as the date-only
/// door: without it, every ms-valued caller of the `"YYYY-MM-DD HH:MM"` form
/// grows its own ms→secs chain with its own fallback. Same **floor**
/// (`div_euclid`) rather than truncate-toward-zero: `-500 ms` is inside second
/// `-1` (…:59 of the previous minute), and `-500 / 1_000` says second `0`.
#[cfg(feature = "local-clock")]
pub fn format_unix_local_ms(ms: i64) -> String {
    format_unix_local(ms.div_euclid(1_000))
}

/// Date-only sibling of [`format_unix_local`] — a local `YYYY-MM-DD` for
/// compact date fields (cert expiry, bunker-connection expiry, the relative-time
/// `≥ 7 d` fallback). `%Y-%m-%d` has no locale-varying component, so this is a
/// shared-Rust target even where a locale-aware short form (e.g. `%b %-d`)
/// stays platform-side. Same never-invent-a-time fallback as
/// [`format_unix_local`]; same `local-clock` gating.
#[cfg(feature = "local-clock")]
pub fn format_unix_local_date(secs: i64) -> String {
    use chrono::{Local, TimeZone as _};
    match Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d").to_string(),
        _ => secs.to_string(),
    }
}

/// Epoch-**millisecond** input adapter over [`format_unix_local_date`], for the
/// many callers whose timestamp is already in ms: the relative-time `≥ 7 d`
/// `Absolute { epoch_ms }` fallback, the backup upload/audit rows'
/// `absolute_epoch_ms` arm, the conversation list's older-message date.
///
/// It exists because those callers are the ones that hand-roll: the seconds
/// entry point above was the only shared door, so every ms-input site grew its
/// own `from_timestamp_millis(..).with_timezone(&Local).format(..)` chain
/// instead — five of them in tui alone, each with a fallback that rendered
/// **empty** where this contract renders the raw number.
///
/// Truncation is **floor**, not toward zero: `-1_500 ms` is inside the second
/// `-2`, and `-1_500 / 1_000` would say `-1` — a whole second late, and on the
/// wrong calendar day for any pre-1970 instant within a second of midnight.
/// Only [`i64::div_euclid`] gets that right, which is precisely the kind of
/// decision that should be made once here rather than at each call site.
#[cfg(feature = "local-clock")]
pub fn format_unix_local_date_ms(ms: i64) -> String {
    format_unix_local_date(ms.div_euclid(1_000))
}

#[cfg(all(test, feature = "local-clock"))]
mod format_unix_local_date_ms_tests {
    use super::{format_unix_local_date, format_unix_local_date_ms};

    /// The ms door agrees with the seconds door on a whole second — the
    /// adapter's whole contract.
    #[test]
    fn agrees_with_the_seconds_door() {
        assert_eq!(
            format_unix_local_date_ms(1_609_459_200_000),
            format_unix_local_date(1_609_459_200)
        );
    }

    /// Sub-second remainders never advance the rendered date: every ms inside
    /// one second renders that second's date.
    #[test]
    fn sub_second_remainders_stay_in_their_second() {
        let base = format_unix_local_date(1_609_459_200);
        for extra_ms in [0, 1, 499, 500, 999] {
            assert_eq!(
                format_unix_local_date_ms(1_609_459_200_000 + extra_ms),
                base,
                "{extra_ms}ms into the second changed the date"
            );
        }
    }

    /// The floor-vs-truncate case, pinned at a ms value where the two actually
    /// render **different dates**.
    ///
    /// ⚠ The obvious version of this test is vacuous, and this one was, until a
    /// mutation round caught it: asserting
    /// `format_unix_local_date_ms(-1_500) == format_unix_local_date(-2)` passes
    /// just as happily when the implementation truncates to `-1`, because two
    /// instants one second apart almost always fall on the same calendar day —
    /// the rendered strings match and the `div_euclid` → `/` mutation survives.
    /// An assertion here has to straddle a **midnight**, not merely a second, or
    /// it is testing nothing. With the mutation applied this now reports
    /// `1970-01-01` where the floor says `1969-12-31`: a whole day wrong.
    ///
    /// The straddling instant is timezone-dependent, so it is searched for rather
    /// than hard-coded: somewhere in any 24h window there is exactly one negative
    /// ms whose floor-second and truncate-second land on different local dates.
    #[test]
    fn negative_remainders_floor_rather_than_truncate_toward_zero() {
        // Every ms here has a -500 remainder, so floor = trunc - 1 second.
        let straddler = (0..86_400i64)
            .map(|k| -(k * 1_000) - 500)
            .find(|&ms| {
                format_unix_local_date(ms.div_euclid(1_000)) != format_unix_local_date(ms / 1_000)
            })
            .expect("some ms in a 24h window must straddle local midnight");

        assert_eq!(
            format_unix_local_date_ms(straddler),
            format_unix_local_date(straddler.div_euclid(1_000)),
            "a negative remainder belongs to the EARLIER second, and here that \
             is a different calendar day — truncating toward zero reports tomorrow"
        );
        assert_ne!(
            format_unix_local_date_ms(straddler),
            format_unix_local_date(straddler / 1_000),
            "this is the assertion the vacuous version of this test was missing"
        );

        // The underlying arithmetic, pinned directly too. (Parenthesized: unary
        // minus binds looser than the method call, so `-1_500_i64.div_euclid(..)`
        // would negate the POSITIVE quotient.)
        assert_eq!((-1_500_i64).div_euclid(1_000), -2);
        assert_eq!(-1_500_i64 / 1_000, -1);
    }

    /// Out of chrono's range the raw number survives — the shared
    /// never-invent-a-time fallback, and specifically NOT the empty string the
    /// hand-rolled ms copies returned (empty is how an *unset* timestamp reads,
    /// so a corrupt one must not be indistinguishable from it).
    #[test]
    fn out_of_range_falls_back_to_the_raw_number_not_empty() {
        let got = format_unix_local_date_ms(i64::MAX);
        assert!(!got.is_empty(), "out-of-range rendered empty");
        assert_eq!(got, format_unix_local_date(i64::MAX.div_euclid(1_000)));
    }
}

#[cfg(all(test, feature = "local-clock"))]
mod format_unix_local_ms_tests {
    use super::{format_unix_local, format_unix_local_ms};

    /// The ms door agrees with the seconds door on a whole second, and
    /// sub-second remainders never advance the rendered minute.
    #[test]
    fn agrees_with_the_seconds_door_across_a_second() {
        let base = format_unix_local(1_609_459_200);
        for extra_ms in [0, 1, 499, 999] {
            assert_eq!(format_unix_local_ms(1_609_459_200_000 + extra_ms), base);
        }
    }

    /// The floor-vs-truncate pin. Unlike the date-only sibling's (which must
    /// straddle a local *midnight*, timezone-dependent, so it searches), the
    /// minute-precision form makes `-500 ms` deterministic in every real
    /// timezone: floor says second `-1` (…:59 of the previous minute), truncate
    /// says second `0` (…:00 of the next) — different rendered minutes because
    /// all real UTC offsets are whole minutes.
    #[test]
    fn negative_remainders_floor_rather_than_truncate_toward_zero() {
        assert_eq!(format_unix_local_ms(-500), format_unix_local(-1));
        assert_ne!(
            format_unix_local_ms(-500),
            format_unix_local(0),
            "truncating toward zero would render the NEXT minute"
        );
    }

    /// Out of chrono's range the raw number survives — never the empty string
    /// (empty is how an unset timestamp reads).
    #[test]
    fn out_of_range_falls_back_to_the_raw_number_not_empty() {
        let got = format_unix_local_ms(i64::MAX);
        assert!(!got.is_empty(), "out-of-range rendered empty");
        assert_eq!(got, format_unix_local(i64::MAX.div_euclid(1_000)));
    }
}

#[cfg(all(test, feature = "local-clock"))]
mod format_unix_local_tests {
    use super::format_unix_local;

    #[test]
    fn date_only_sibling_formats_as_year_month_day() {
        // Same instant as the datetime test below; the local render varies by
        // machine timezone, but the date-only shape never does.
        let out = super::format_unix_local_date(1_609_459_200);
        assert_eq!(out.len(), 10, "expected \"YYYY-MM-DD\": {out}");
        assert_eq!(&out[4..5], "-");
        assert_eq!(&out[7..8], "-");
    }

    #[test]
    fn formats_as_year_month_day_hour_minute() {
        // 2021-01-01T00:00:00 UTC — the local render varies by machine
        // timezone, but the shape (fixed-width, minute-precision) never does.
        let out = format_unix_local(1_609_459_200);
        assert_eq!(out.len(), 16, "expected \"YYYY-MM-DD HH:MM\": {out}");
        assert_eq!(&out[4..5], "-");
        assert_eq!(&out[7..8], "-");
        assert_eq!(&out[10..11], " ");
        assert_eq!(&out[13..14], ":");
    }

    #[test]
    fn falls_back_to_raw_seconds_when_out_of_range() {
        // Far beyond chrono's representable range: LocalResult::None, not a
        // panic — the fallback path, not an invented time.
        let out = format_unix_local(i64::MAX);
        assert_eq!(out, i64::MAX.to_string());
    }
}

/// Recursively walk `src_dir`'s `.rs` files and report every `.format("<literal>")`
/// call whose literal (whitespace-collapsed, so a rustfmt-wrapped multi-line
/// call still reads as one span) matches one of `needles` — each a
/// `(format_str, owner_description)` pair naming the shared-Rust fn that
/// literal should have come from instead. One message per offender, sorted.
///
/// For an app's own `no_painted_text_hand_rolls_*` self-check test: was
/// hand-duplicated byte-for-byte between tui and linux (each necessarily
/// scoped to its own `env!("CARGO_MANIFEST_DIR")`, which is why this stayed
/// two copies instead of one test) — only the walk+match logic is common, so
/// that is what moved here; each caller still builds its own `needles` from
/// the shared fns' actual output (so a format-string change can't silently
/// desync the check from the code it's checking) and supplies its own source
/// root. Native-only: a source-tree walk has no wasm32 meaning.
#[cfg(not(target_arch = "wasm32"))]
pub fn find_hand_rolled_format_calls(
    src_dir: &std::path::Path,
    needles: &[(&str, &str)],
) -> Vec<String> {
    let mut offenders: Vec<String> = Vec::new();
    let mut stack = vec![src_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read source");
            let flat: String = text.split_whitespace().collect::<Vec<_>>().join("");
            let rel = path
                .strip_prefix(src_dir)
                .unwrap_or(&path)
                .to_str()
                .expect("utf-8 path")
                .replace('\\', "/");
            for span in flat.split(".format(\"").skip(1) {
                let Some((fmt, _)) = span.split_once('"') else {
                    continue;
                };
                for (needle, owner) in needles {
                    let collapsed: String = needle.split_whitespace().collect::<Vec<_>>().join("");
                    if fmt == collapsed {
                        offenders.push(format!(
                            "{rel}: .format(\"{needle}\") re-implements {owner}"
                        ));
                    }
                }
            }
        }
    }
    offenders.sort();
    offenders
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod find_hand_rolled_format_calls_tests {
    use super::find_hand_rolled_format_calls;

    /// A `.rs` file whose `.format("%Y-%m-%d")` call matches a needle is
    /// reported, with the message naming both the file and the needle's owner.
    #[test]
    fn reports_a_matching_format_call_with_its_owner() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("offender.rs"),
            "fn f(d: chrono::DateTime<chrono::Local>) -> String { d.format(\"%Y-%m-%d\").to_string() }",
        )
        .expect("write");
        let offenders = find_hand_rolled_format_calls(
            dir.path(),
            &[("%Y-%m-%d", "fauna_core::format::format_unix_local_date")],
        );
        assert_eq!(offenders.len(), 1);
        assert!(offenders[0].contains("offender.rs"));
        assert!(offenders[0].contains("fauna_core::format::format_unix_local_date"));
    }

    /// A rustfmt-wrapped call (the literal on its own indented line) is still
    /// caught — the whole point of collapsing whitespace before matching.
    #[test]
    fn catches_a_rustfmt_wrapped_call() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("offender.rs"),
            "fn f(d: chrono::DateTime<chrono::Local>) -> String {\n    d.format(\n        \"%Y-%m-%d\",\n    )\n    .to_string()\n}\n",
        )
        .expect("write");
        let offenders = find_hand_rolled_format_calls(dir.path(), &[("%Y-%m-%d", "the shared fn")]);
        assert_eq!(offenders.len(), 1);
    }

    /// A `.format(...)` call whose literal matches no needle is not reported.
    #[test]
    fn a_non_matching_format_call_is_not_an_offender() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("clean.rs"),
            "fn f(x: i32) -> String { format!(\"{x}\") }",
        )
        .expect("write");
        let offenders = find_hand_rolled_format_calls(dir.path(), &[("%Y-%m-%d", "owner")]);
        assert!(offenders.is_empty());
    }
}

/// Format `bytes` as the largest 1024-unit whose scaled value is `≥ 1`, to at
/// most one decimal (trailing `.0` dropped), as an i18n key + `{value}` arg.
/// See `docs/goal/behavior/value-formatting.md` § Byte sizes.
pub fn byte_size(bytes: u64) -> LocalizedText {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    const TIB: f64 = GIB * 1024.0;
    let b = bytes as f64;
    let (key, value) = if b < KIB {
        ("size.bytes", b)
    } else if b < MIB {
        ("size.kb", b / KIB)
    } else if b < GIB {
        ("size.mb", b / MIB)
    } else if b < TIB {
        ("size.gb", b / GIB)
    } else {
        ("size.tb", b / TIB)
    };
    LocalizedText::key_arg(key, "value", fmt_one_decimal(value))
}

/// Millisatoshis as a tip amount (`monetization.md` § Tips) — the display
/// half of the post tip surface, rendered into `post-tip-total`.
///
/// **Sats are the display unit; msats are the wire unit.** § The asking price
/// already fixes that split for the author's input ("the author types the price
/// in sats and one shared parse/format pair converts to the msat wire value"),
/// and a reader's total is the same split read backwards — so a tip of
/// `21_000` msats reads "21 sats", never "21000".
///
/// Sub-sat amounts keep their own unit rather than rounding to "0 sats": a
/// 500-msat tip is real money and a zero would be a lie about it. That is the
/// same reason [`byte_size`] keeps bytes below a KiB instead of showing "0 KB".
///
/// | Range | i18n key |
/// |---|---|
/// | `< 1 sat` (`< 1000 msat`) | `tips.msats` → "{value} msats" |
/// | `≥ 1 sat` | `tips.sats` → "{value} sats" |
///
/// Scaled to at most one decimal with a trailing `.0` dropped, exactly like
/// [`byte_size`]; unit symbols are locale-invariant.
///
/// **Callers render this only for a non-zero total** — `post-tip-total` is
/// absent at zero, because a post whose every receipt carried an unparseable
/// invoice has real tips and no amount, and "0 sats" would misreport that as
/// "nobody paid" (`monetization.md` § Tips — an absent amount is a real state,
/// never coerced to 0). The function still answers for `0`, so a caller that
/// asks gets "0 msats" rather than a panic or an empty string.
pub fn tip_amount(msats: i64) -> LocalizedText {
    // The integer constant is authoritative; the cast is exact at this value.
    const MSATS_PER_SAT: f64 = crate::money::MSATS_PER_SAT as f64;
    let m = msats as f64;
    if m.abs() < MSATS_PER_SAT {
        LocalizedText::key_arg("tips.msats", "value", fmt_one_decimal(m))
    } else {
        LocalizedText::key_arg("tips.sats", "value", fmt_one_decimal(m / MSATS_PER_SAT))
    }
}

/// How many tips a post has (`monetization.md` § Tips), rendered into
/// `post-tip-count`.
///
/// Shared rather than per-app because the singular is a **decision**, not a
/// formatting detail: seven apps each picking one is seven chances to ship
/// "1 tips". The codebase has no plural machinery, so the two forms are two
/// keys — the `time.hour_1` / `reminder` precedent.
///
/// Counts every tip on the post, **including those that reported no amount**,
/// which is why this can be non-zero while [`tip_amount`]'s total is zero.
pub fn tip_count(count: i64) -> LocalizedText {
    if count == 1 {
        LocalizedText::key("tips.count_one")
    } else {
        LocalizedText::key_arg("tips.count", "count", count.to_string())
    }
}

/// The `post-tip-list` tail, "and N more", for a bounded attribution window
/// (`monetization.md` § Tips) — the `post-tip-item` sibling of [`tip_amount`]/
/// [`tip_count`], shared for the same reason: a per-app re-derivation is a
/// chance to disagree with the nest's own `has_more` + totals.
///
/// `n` is the caller's `tip_count - senders.len()`, computed from the nest's
/// own `has_more`/totals rather than comparing the rendered row count against
/// a cap the client hard-codes. Negative input clamps to zero.
pub fn tip_more(n: i64) -> LocalizedText {
    LocalizedText::key_arg("tips.more", "count", n.max(0).to_string())
}

/// How many events fall on a calendar day (the month-grid day-cell
/// accessibility tooltip, `events-day-cell`) — shared for the same reason as
/// [`tip_count`]: the singular is a decision, not a formatting detail, and
/// the codebase has no plural machinery, so the two forms are two keys.
/// Callers render the count clause only when `count > 0` — a dayless clause
/// is a caller-side choice, not this function's.
pub fn event_count(count: i64) -> LocalizedText {
    if count == 1 {
        LocalizedText::key("events.event_count_one")
    } else {
        LocalizedText::key_arg("events.event_count", "count", count.to_string())
    }
}

/// Parse a user-typed storage size — the inverse of [`byte_size`], and the one
/// the `backup-destination-capacity-input` cap is read through on every app.
///
/// Deliberately liberal about what a person types and strict about what it
/// accepts as a *number*: `"50 GB"`, `"50GB"`, `"50 gb"`, `"1.5 TB"`, `"50 GiB"`
/// and a bare `"1024"` (bytes) all parse; `""`, `"lots"`, `"-5 GB"` and
/// `"5 bananas"` do not. Units are **1024-based**, matching [`byte_size`]'s own
/// scaling, so a value round-trips through the two functions unchanged — a cap
/// the user typed must not drift every time the page repaints it.
///
/// Returns `None` rather than a default for anything it cannot read: the caller
/// (a shell's confirm button) surfaces that as a refusal, because silently
/// substituting a cap the user did not choose is exactly the class of guess that
/// fills a device's disk.
///
/// See `docs/goal/behavior/backup-destinations.md` § Third destination kind — the capacity cap is
/// the client-device kind's only knob.
pub fn parse_byte_size(input: &str) -> Option<u64> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    // Split the leading numeric run from the trailing unit. `char::is_numeric`
    // would accept non-ASCII digits that `f64::from_str` then rejects, so the
    // split and the parse agree on ASCII.
    let split = s
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != ',')
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    // Accept a decimal comma, the separator most of the world types.
    let value: f64 = num.trim().replace(',', ".").parse().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let unit = unit.trim().to_ascii_lowercase();
    let multiplier: f64 = match unit.as_str() {
        "" | "b" | "byte" | "bytes" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024.0 * 1024.0,
        "g" | "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    let bytes = value * multiplier;
    // Past u64 the value is not a storage cap anyone meant; refuse rather than
    // saturate to "effectively uncapped", which is the opposite of the intent.
    if bytes > u64::MAX as f64 {
        return None;
    }
    Some(bytes as u64)
}

/// One-decimal formatting with a trailing `.0` dropped (`1.5`, `512`, `2`).
fn fmt_one_decimal(v: f64) -> String {
    let s = format!("{v:.1}");
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

/// Format a duration in whole seconds as a coarse `d`/`h`/`m` string (uptime
/// style): the largest non-zero unit down to minutes, always showing the full
/// chain below it. Sub-minute durations render as `0m`. Returned as an i18n key
/// (`time.uptime_dhm` / `_hm` / `_m`) plus `{days}`/`{hours}`/`{mins}` args, so
/// no English is baked in. See `docs/goal/behavior/value-formatting.md` § Duration.
pub fn duration_secs(secs: u64) -> LocalizedText {
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let mins = (secs % 3_600) / 60;
    let mut lt = if days > 0 {
        let mut lt = LocalizedText::key_arg("time.uptime_dhm", "days", days.to_string());
        lt.args.insert("hours".to_string(), hours.to_string());
        lt
    } else if hours > 0 {
        LocalizedText::key_arg("time.uptime_hm", "hours", hours.to_string())
    } else {
        LocalizedText::key("time.uptime_m")
    };
    lt.args.insert("mins".to_string(), mins.to_string());
    lt
}

/// Countdown to a future deadline (both epoch-ms), coarse `d`/`h` (uptime-style,
/// no minutes — the deadline is days out and the caller re-renders on each
/// snapshot refresh, not a per-second ticker). `Some(LocalizedText)` (keys
/// `time.countdown_dh` / `time.countdown_h`) while `deadline_ms > now_ms`;
/// `None` once elapsed — the caller renders its own already-localized
/// "elapsed" label (e.g. the mail primary-domain-rename banner's
/// `admin.dns.rename.grace_elapsed`), since that label's wording is
/// per-surface, not a generic countdown concern. Unifies the byte-identical
/// per-app hand-rolls (web `graceRemaining`, linux `grace_remaining`,
/// apple/android/windows equivalents) — every one hardcoded the `d`/`h` unit
/// letters, a priority #1 zero-hardcoded-English violation on all five — onto
/// one source of truth, mirroring [`duration_secs`]'s d/h/m shape. See
/// `docs/goal/behavior/value-formatting.md` § Grace countdown.
pub fn grace_countdown(deadline_ms: i64, now_ms: i64) -> Option<LocalizedText> {
    let ms = deadline_ms - now_ms;
    if ms <= 0 {
        return None;
    }
    let total_hours = ms / 3_600_000;
    let days = total_hours / 24;
    let hours = total_hours % 24;
    Some(if days > 0 {
        let mut lt = LocalizedText::key_arg("time.countdown_dh", "days", days.to_string());
        lt.args.insert("hours".to_string(), hours.to_string());
        lt
    } else {
        LocalizedText::key_arg("time.countdown_h", "hours", hours.to_string())
    })
}

/// Canonical short display form for a long hex id — a 64-hex Fauna actor-id
/// or nest-id: the first 12 characters followed by a single-character ellipsis
/// `…` (`U+2026`). Ids of 12 or fewer characters are returned unchanged.
/// Unifies the per-app truncations (web `hex.slice(0,12)+'...'`, iOS
/// `.prefix(12)+"..."`, Android `.take(12)+"..."`, Linux `&hex[..12]+'…'`)
/// onto one shape — priorities #1 (minimize divergence) / #4 (resolve drift).
/// See `docs/goal/behavior/value-formatting.md` § Short id.
pub fn short_id(hex: &str) -> String {
    elide_hex(hex, SHORT_ID_WIDTH)
}

/// The width [`short_id`] keeps — the canonical short display width for a long
/// hex id, and the floor [`device_display_identities`] widens up from.
const SHORT_ID_WIDTH: usize = 12;

/// `hex` truncated to `width` characters with a single-character ellipsis `…`
/// (`U+2026`) marking the elision; returned unchanged when nothing is elided.
/// The shape [`short_id`] fixes at [`SHORT_ID_WIDTH`] and
/// [`device_display_identities`] widens on demand.
fn elide_hex(hex: &str, width: usize) -> String {
    if hex.chars().count() <= width {
        hex.to_string()
    } else {
        let prefix: String = hex.chars().take(width).collect();
        format!("{prefix}…")
    }
}

/// Head…tail elision of a long hex id (a 64-hex `nest_actor_id`) for a
/// recovery-box row label: the first 8 characters, a single-character
/// ellipsis `…` (`U+2026`), and the last 8 characters. Ids of 20 or fewer
/// characters are returned unchanged. Unifies the per-app box-recovery
/// truncations (web `shortNestId`, Linux `short_nest_id`, Windows
/// `ShortNestId` — the Windows implementation's own comment already flagged
/// it as "a candidate future shared-Rust lift") onto one shape — priorities
/// #1 (minimize divergence) / #2 (shared Rust). Distinct from [`short_id`],
/// which keeps only a 12-char *prefix* (no tail) and is used for the
/// handle-less account/actor fallback label, not a box-recovery row. See
/// `docs/goal/behavior/value-formatting.md` § Short nest id.
pub fn short_nest_id(id: &str) -> String {
    if id.len() > 20 {
        format!("{}…{}", &id[..8], &id[id.len() - 8..])
    } else {
        id.to_string()
    }
}

/// The short display form of a **fleet id** — a fleet member's 32-byte
/// device principal — on the Devices page: the first 8 hex characters, a
/// single-character ellipsis `…` (`U+2026`), and the last 8 (`3f9a1b2c…c21e9d8f`).
///
/// It is the ONE identity a client holds for a fleet member that no roster
/// row accounts for (`account-data-taxonomy.md` § The generation machinery →
/// *Fleet-scope reclamation*, clause (4), *A disagreement is the user's to
/// settle*): such a member has no nest row, so no device label. The user
/// settles a removal **by elimination** — every device still in hand shows
/// its own fingerprint (`device-own-fingerprint`) and the member to remove
/// is the card (`device-member-fingerprint`) matching none of them — which
/// makes the two surfaces two halves of ONE comparison: both render through
/// this function and nothing else, at one width and one elision. Were they
/// ever to differ, "matches none" would fire on a device the user holds.
///
/// **Why 8+8 and not shorter.** The width is a security parameter, not a
/// taste: an attacker holding a stolen device can re-mint its principal
/// until the fingerprint collides with a live sibling's, and a collision
/// makes the stolen card read as "a device you hold" (the honest re-minted
/// case the note warns about). Sixteen hex characters put that search at
/// 2^64 trials; the four-and-four the design sketch showed would have been
/// 2^32, a laptop's afternoon. Plain `String`, locale-invariant. See
/// `docs/goal/behavior/value-formatting.md` § Fleet fingerprint.
pub fn fleet_fingerprint(id: &[u8; 32]) -> String {
    let hex = crate::hex32::encode(id);
    format!(
        "{}…{}",
        &hex[..FLEET_FINGERPRINT_EDGE],
        &hex[hex.len() - FLEET_FINGERPRINT_EDGE..]
    )
}

/// Hex characters [`fleet_fingerprint`] keeps at each end.
const FLEET_FINGERPRINT_EDGE: usize = 8;

/// The account-switcher row title: the account's cached handle when present
/// and non-empty, else the canonical [`short_id`] of its actor id (usable
/// before the first server-data cache refresh). Unifies the per-app
/// fallback (Linux's richer `handle.filter(!empty).unwrap_or(12-char-prefix +
/// "…")`, which web's `handle ?? actor_id.slice(0, 12)` had drifted from —
/// no ellipsis, and `??` does not treat an empty string as absent) onto
/// Linux's richer shape — priorities #1 (minimize divergence) / #4 (resolve
/// drift, richest pattern wins). See `docs/goal/behavior/value-formatting.md`
/// § Account display label.
pub fn account_display_label(handle: Option<&str>, actor_id: &str) -> String {
    handle
        .filter(|h| !h.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| short_id(actor_id))
}

/// The canonical `handle@domain` a principal is named by, or `None` when no
/// handle was served (absent or empty). An absent or empty domain leaves the
/// handle bare — which, everywhere in the apps, means one of the viewer's own
/// nest's users; a handle with a domain beside it is a foreign user the
/// viewer's own nest bound to that domain's key. The pair crosses every wire
/// split and is joined only here, so the conversation roster and the folder
/// surfaces cannot drift; the result feeds [`account_display_label`] as the
/// handle. See `docs/goal/behavior/value-formatting.md` § Account display
/// label.
pub fn qualified_handle(handle: Option<&str>, domain: Option<&str>) -> Option<String> {
    let handle = handle.filter(|h| !h.is_empty())?;
    Some(match domain.filter(|d| !d.is_empty()) {
        Some(domain) => format!("{handle}@{domain}"),
        None => handle.to_string(),
    })
}

/// What to call another person — [`peer_display_label`]'s answer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PeerLabel {
    /// The one line every surface shows.
    pub primary: String,
    /// `Some` only when the viewer's own nickname supplied [`Self::primary`]:
    /// what `primary` would have been without it, for the secondary line a
    /// list or detail surface renders so a private name never hides the
    /// public identity.
    pub public: Option<String>,
}

/// The **one** answer to "what do I call this other person", for every
/// surface naming someone who is not the viewer: the viewer's own
/// **nickname** for them (the private contact overlay), else their
/// self-published **display name**, else exactly [`account_display_label`]
/// (handle, else [`short_id`]). Each text input counts only when non-empty
/// after trimming. With `nickname = None, display_name = None` the primary is
/// byte-identical to [`account_display_label`]. See
/// `docs/goal/behavior/value-formatting.md` § Peer display label.
pub fn peer_display_label(
    nickname: Option<&str>,
    display_name: Option<&str>,
    handle: Option<&str>,
    actor_id: &str,
) -> PeerLabel {
    fn present(s: Option<&str>) -> Option<&str> {
        s.map(str::trim).filter(|s| !s.is_empty())
    }
    let public = present(display_name)
        .map(str::to_string)
        .unwrap_or_else(|| account_display_label(handle, actor_id));
    match present(nickname) {
        Some(nick) => PeerLabel {
            primary: nick.to_string(),
            public: Some(public),
        },
        None => PeerLabel {
            primary: public,
            public: None,
        },
    }
}

/// Resolve a profile-navigation target: `None` when `entry_actor_id` names the
/// viewer themselves (open the SELF profile), `Some(trimmed id)` otherwise
/// (open that OTHER actor's profile). A test-agent `nav` patch carrying a bare
/// actor id has no other way to say "this happens to be you" — production
/// navigation (a contact tap-through, a mention) always already knows which
/// case it is, so this exists only where an id arrives without that context.
///
/// Was three separate hand-rolled copies (linux, tui, android) before this lift
/// (priority #2) — linux's is the reference this ports verbatim
/// (`apps/fauna-linux/src/test_agent.rs::profile_nav_target`, 6 unit-pinned
/// cases). Self-detection trims both sides and compares ASCII-case-insensitively
/// (hex is case-normalized inconsistently across callers); an **unknown**
/// `self_actor_id` (`None`/empty — pre-`AccountLoaded`) never falls back to
/// self, since a named target must still open rather than silently resolve to
/// "probably me". See `docs/goal/ui/profile.md` § Layout & flow → Another's
/// profile.
pub fn profile_nav_target(entry_actor_id: &str, self_actor_id: Option<&str>) -> Option<String> {
    let target = entry_actor_id.trim();
    if target.is_empty() {
        return None;
    }
    let is_self = self_actor_id
        .map(str::trim)
        .is_some_and(|me| !me.is_empty() && me.eq_ignore_ascii_case(target));
    if is_self {
        None
    } else {
        Some(target.to_string())
    }
}

#[cfg(test)]
mod profile_nav_target_tests {
    use super::*;

    const ME: &str = "aa11bb22cc33dd44";
    const OTHER: &str = "ff99ee88dd77cc66";

    #[test]
    fn an_other_actor_id_resolves_to_that_actor() {
        assert_eq!(profile_nav_target(OTHER, Some(ME)), Some(OTHER.to_string()));
    }

    #[test]
    fn the_viewers_own_actor_id_resolves_to_the_self_profile() {
        assert_eq!(profile_nav_target(ME, Some(ME)), None);
    }

    #[test]
    fn self_detection_ignores_hex_case_and_surrounding_space() {
        assert_eq!(profile_nav_target("  AA11BB22CC33DD44 ", Some(ME)), None);
    }

    #[test]
    fn an_other_actor_id_keeps_its_trimmed_form() {
        assert_eq!(
            profile_nav_target(&format!("  {OTHER}  "), Some(ME)),
            Some(OTHER.to_string())
        );
    }

    #[test]
    fn a_blank_actor_id_resolves_to_the_self_profile() {
        assert_eq!(profile_nav_target("", Some(ME)), None);
        assert_eq!(profile_nav_target("   ", Some(ME)), None);
    }

    #[test]
    fn an_unknown_viewer_identity_still_opens_the_named_actor() {
        assert_eq!(profile_nav_target(OTHER, None), Some(OTHER.to_string()));
        assert_eq!(profile_nav_target(OTHER, Some("")), Some(OTHER.to_string()));
    }
}

/// The display identities of **one owner's whole device list**, for a surface
/// whose reader holds no key for the registering owner's label root — today the
/// guardian's ward-device rows (`FamilyWardDeviceInfo.label`, populated nest-side
/// in the `fauna.family.status` projection). Returns one string per input device,
/// in order.
///
/// Each row is the device's **code** — the hex `device_id`, elided to the
/// narrowest width that tells every device in this list apart (from
/// [`SHORT_ID_WIDTH`] up to the full id) — with the machine-authored plaintext
/// label, when one rests, rendered beside it. NEVER the owner's user-chosen
/// label: that rests sealed under the owner's own root, which a guardian neither
/// holds nor may be handed (`family-safety.md` § Don't do these — no guardian key
/// escrow; the ruling is § Full visibility for young children, 2026-08-02).
///
/// ⚠ **This takes the whole set on purpose, and must not be reduced to a
/// per-device function.** `family-safety.md` § Full visibility guarantees the
/// rows are *per-device distinct*, and distinctness is a property of the set: no
/// function handed one device can promise its output differs from a row it never
/// sees. The predecessor `device_display_identity(label, device_id)` had exactly
/// that signature and could not keep the guarantee — `device_id` is **chosen by
/// the client**, not derived (`fauna.sync.register` stores the caller's hex
/// verbatim), so a ward who is shown the guardian's enrolled device in their own
/// `devices.list` could register a decoy agreeing on the 12 characters rendered
/// and produce a byte-identical row; and two devices carrying the same
/// machine-authored label (`SELF_REGISTER_LABEL` — what *every* self-registering
/// client passes) collided with no adversary at all.
///
/// Widening only on a real collision is deliberate: a fixed 12 is forgeable, a
/// fixed 64 is unreadable on every row forever, and set-relative width pays the
/// legibility cost exactly when an ambiguity exists. An owner can force the width
/// up on their own list, which is visible and never misleading. See
/// `docs/goal/behavior/value-formatting.md` § Device display identity.
pub fn device_display_identities<'a>(
    devices: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Vec<String> {
    let rows: Vec<(&str, String)> = devices
        .into_iter()
        .map(|(label, device_id)| (label, hex_full(device_id)))
        .collect();
    let width = distinguishing_id_width(rows.iter().map(|(_, hex)| hex.as_str()));
    rows.iter()
        .map(|(label, hex)| {
            let code = elide_hex(hex, width);
            if label.is_empty() {
                code
            } else {
                format!("{label} · {code}")
            }
        })
        .collect()
}

/// The narrowest elision width (from [`SHORT_ID_WIDTH`], in steps of 4) at which
/// every id in `hexes` is distinct, capped at the longest id present.
///
/// Termination is structural: `device_id` is the primary key of a row within one
/// owner's device list, so the full ids are already distinct and the cap always
/// separates them. A caller passing genuine duplicates gets the capped width and
/// duplicate output — the honest answer, since no rendering can distinguish two
/// identical ids.
fn distinguishing_id_width<'a>(hexes: impl Iterator<Item = &'a str> + Clone) -> usize {
    let longest = hexes
        .clone()
        .map(|hex| hex.chars().count())
        .max()
        .unwrap_or(0);
    let mut width = SHORT_ID_WIDTH;
    while width < longest {
        let mut seen = std::collections::BTreeSet::new();
        if hexes
            .clone()
            .all(|hex| seen.insert(hex.chars().take(width).collect::<String>()))
        {
            return width;
        }
        width += 4;
    }
    longest
}

/// The subscription row's author label: the creator's nest-resolved handle when
/// present and non-blank, else the [`hex_full`] form of their actor id. Unifies
/// the per-app chooser (linux/apple/android/windows each tested only
/// "non-empty", so a whitespace-only handle rendered a blank row; web alone
/// trimmed) onto web's richer trimming shape — priorities #1 (minimize
/// divergence) / #4 (resolve drift, richest pattern wins). The handle is
/// returned trimmed, so stray padding never reaches the row.
///
/// **Distinct from [`account_display_label`]**, which falls back to [`short_id`]
/// (a 12-char elision) for the account-switcher title. This site shows the
/// **full** hex id: it is the creator's canonical copyable identity when no
/// handle resolves. Pre-computed at each transcribe rather than exposed over
/// UniFFI/wasm — see `docs/goal/behavior/value-formatting.md` § Subscription
/// author label.
pub fn author_display_label(handle: Option<&str>, author_id: &[u8]) -> String {
    handle
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| hex_full(author_id))
}

/// Short hex display of an id given as raw bytes — the first 4 bytes rendered as
/// 8 lowercase, zero-padded hex characters (fewer when the input is shorter; an
/// empty slice yields `""`). The shared label behind every app's non-local
/// backup-restore source (a restore-history row's `source_member_id` → the
/// destination member's short hex), unifying the per-app copies (Linux
/// `hex_short`, Android `hexShort`, Windows `HexShort`, web `hexShort`) onto one
/// shape — priorities #1 (minimize divergence) / #4 (resolve drift). Distinct
/// from [`short_id`], which truncates a 64-hex *string* to 12 chars + `…`. See
/// `docs/goal/ui/backups.md` § Where logic lives.
pub fn hex_short(bytes: &[u8]) -> String {
    bytes.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// Full hex display of an id given as raw bytes — every byte rendered as two
/// lowercase, zero-padded hex characters (an empty slice yields `""`). The
/// canonical string form of an actor/member id shown as the fallback label when
/// no handle is available (and everywhere a full 64-hex id string is displayed
/// or copied), unifying the per-app copies (Linux `.to_hex()`, Android
/// `HexUtil.bytesToHex`, Windows `Convert.ToHexString(..).ToLowerInvariant()`,
/// web `bytesToHex`, apple `hexString`) onto one shape — priorities #1 (minimize
/// divergence) / #4 (resolve drift). The full-length sibling of [`hex_short`]
/// (first 4 bytes); distinct from [`short_id`], which truncates a 64-hex
/// *string* to 12 chars + `…`. See `docs/goal/behavior/value-formatting.md`.
pub fn hex_full(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The decode counterpart to [`hex_full`] — any even-length hex string (either
/// case, per [`u8::from_str_radix`]) to bytes; `None` on odd length or a
/// non-hex byte, never a panic. Unifies the identical hand-rolled
/// `step_by(2)` + `from_str_radix` loop that had been copied into
/// `fauna-client-family::supervision_snapshot`, `fauna-ffi::content_index_session`,
/// and `fauna-client-recovery`'s `recovery_fixture` example (priorities #1
/// minimize divergence / #2 shared Rust). Deliberately **not** adopted by
/// `fauna-sni-router::decode_hex` (byte-identical) — that binary has no
/// `fauna-core` dependency by design (its own doc comment: "dependency-free
/// splicer ethos") — nor by `fauna-mail::msgid::decode_hex_lower`, which
/// rejects uppercase on purpose (a stricter, genuinely different contract).
pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Whole-percent display of a moderation classifier's `confidence_per_mille`
/// (`0..=1000` — the dag-cbor wire form, since floats are forbidden), rounded
/// **half-up**: `(per_mille + 5) / 10`. So `920 ‰ → 92 %`, `925 ‰ → 93 %`,
/// `995 ‰ → 100 %`. The single rounding contract behind the moderation-queue
/// `confidence` column, unifying the per-app copies that each re-derived the
/// same integer math with explicit "mirror" comments — Linux
/// `(action.confidence_per_mille + 5) / 10` (`views/moderation.rs`), Windows
/// `(a.confidencePerMille + 5) / 10` (`ModerationViewModel`), apple
/// `(UInt32(perMille) + 5) / 10` (`ModerationQueueVM`) — onto one source of
/// truth, so no client silently drifts to truncation on a shown number
/// (priority #1 minimize divergence / #4 resolve drift). Widened to `u32` so an
/// out-of-range wire value cannot overflow; a well-formed `0..=1000` input
/// yields `0..=100`. See `docs/goal/behavior/value-formatting.md` § Confidence
/// percent and `docs/goal/behavior/moderation.md` § Where logic lives.
pub fn confidence_percent(per_mille: u16) -> u32 {
    (u32::from(per_mille) + 5) / 10
}

/// A `{used_bytes, max_bytes}` usage pair (`fauna_protocol::account::UsageBytes`,
/// the `i64` wire form) reduced to a `0.0..=1.0` bar-fill fraction. Guarded
/// against `max_bytes <= 0` (returns `0.0` — no divide-by-zero) and against a
/// negative `used_bytes`; clamped to `1.0` so an over-quota row (usage that has
/// since crept past the cap) never overflows the bar. The single computation
/// behind every app's storage/quota progress indicator, unifying the
/// per-app copies that each guarded (or didn't) the zero/over-quota edges
/// slightly differently — windows `total > 0 ? used / total : 0` (no clamp,
/// `SettingsViewModel.StoragePercent`), android `if (limit > 0) used / limit
/// else 0f` + `.coerceIn(0f, 1f)` (`AccountSettingsScreen`), apple `guard max >
/// 0 else { return 0 }; min(used / max, 1.0)` (`QuotaBar`), and web `used /
/// max_bytes` with **no** zero-guard (`+page.svelte` — `max_bytes == 0`
/// produces `NaN`, a real bug this closes) — onto one source of truth
/// (priority #1 minimize divergence / #2 shared Rust / #4 resolve drift). See
/// [`quota_percent`] for the whole-percent sibling and
/// `docs/goal/behavior/value-formatting.md` § Quota fraction.
pub fn quota_fraction(used_bytes: i64, max_bytes: i64) -> f64 {
    if max_bytes <= 0 {
        return 0.0;
    }
    (used_bytes.max(0) as f64 / max_bytes as f64).min(1.0)
}

/// Whole-percent sibling of [`quota_fraction`], rounded to the nearest percent
/// (`(fraction * 100.0).round()`) for a text label (e.g. android's `"{pct}%"`)
/// rather than a bar-fill value. Inherits the same zero/negative/over-quota
/// guards.
pub fn quota_percent(used_bytes: i64, max_bytes: i64) -> u32 {
    (quota_fraction(used_bytes, max_bytes) * 100.0).round() as u32
}

/// Best-effort host extraction from a `scheme://host[:port]/…` URL. `None`
/// when no host can be isolated (empty input, or a bare `scheme://` with
/// nothing after it) — the caller decides what "no host" means for its own
/// contract (omit a qualifier, disable a feature, …), unlike [`url_host`]'s
/// echo-the-input fallback, which exists for a display label where showing
/// *something* beats showing nothing.
///
/// **Any `userinfo@` is dropped** ([`crate::web::strip_userinfo`]) before the
/// port split, because this is a *display* helper whose output every app shows
/// as "the domain this link goes to" (link-preview chips, backup-destination
/// labels, nest qualifiers). Splitting on `:` and `/` together used to stop at
/// the first colon and never look for an `@`, so
/// `https://example.com:x@evil.com/` rendered as **`example.com`** — a link to
/// `evil.com` wearing a trusted domain's name, on all 7 apps at once.
///
/// The authority is isolated via [`crate::web::generic_authority`] — shared
/// with `fauna_onboarding_machine::helpers::nest_host` and
/// `fauna_core::resolve::parse_node_address` — so it ends at the same four
/// WHATWG terminators (`/ \ ? #`) those extractors do, not just `/`:
/// `https://evil.example?@example.com/` used to render `example.com`, the same
/// display spoof as the userinfo case above.
pub fn url_host_opt(url: &str) -> Option<String> {
    let authority = crate::web::generic_authority(url);
    let host = crate::web::strip_userinfo(authority)
        .split(':')
        .next()
        .unwrap_or("");
    (!host.is_empty()).then(|| host.to_string())
}

/// Best-effort host extraction from a `scheme://host[:port]/…` URL, for a
/// display label — no `url`-crate dependency just to drop the scheme/port/path.
/// Returns the original `url` when no host can be isolated (empty or
/// scheme-only; see [`url_host_opt`] for the `None`-on-failure sibling).
/// Unifies the per-app URL-host parses (linux `url_host`/`nest_url_host`,
/// windows `UrlHost`, apple `urlHost`) onto one shape — priorities #1
/// (minimize divergence) / #4 (resolve drift).
pub fn url_host(url: &str) -> String {
    url_host_opt(url).unwrap_or_else(|| url.to_string())
}

/// Parse a user-entered port string into a valid TCP port (`1..=65535`).
/// Trims surrounding whitespace; returns `None` for empty, non-numeric,
/// negative, fractional, or out-of-range input — notably `0`, which is not a
/// bindable
/// listener port (`libs/fauna-protocol/src/wrapped_blob.rs` § CalDAV listener
/// port: "1–65535; 0 is rejected"). The single validator behind the admin
/// CalDAV- and serving-port fields, unifying the per-app range checks
/// (windows `ushort.TryParse(..) || port < 1`, android `port == null || port < 1
/// || port > 65535`, apple's equivalent) onto one shape — priorities #1
/// (minimize divergence) / #2 (shared Rust) / #4 (resolve drift). Clients render
/// their existing `*_PORT_INVALID` i18n string on `None`.
pub fn parse_port(input: &str) -> Option<u16> {
    match input.trim().parse::<u16>() {
        Ok(port) if port >= 1 => Some(port),
        _ => None,
    }
}

/// Parse a user-entered admin **tier-cap** field (the `admin-settings-tier-cap-*`
/// inbox / storage / devices / blob-size / feeds inputs) into a non-negative
/// `i64`. Trims surrounding whitespace; a negative value **clamps to `0`** (not a
/// fallback — matching the apps' coerce-to-non-negative); returns `None` for
/// empty, non-numeric, fractional, or out-of-`i64`-range input. Callers consume it
/// as `parse_cap(text).unwrap_or(prev)`, so an empty/unparseable edit keeps the
/// persisted value and never silently zeroes a cap. Unlike [`parse_port`], `0` is
/// a valid cap (an explicit "no allowance"). The single validator behind every
/// app's tier-cap save, unifying the per-app parses (linux
/// `parse::<i64>().unwrap_or(prev).max(0)`, android `toLongOrNull()?.coerceAtLeast(0)
/// ?: prev`, windows `long.TryParse(..) ? Math.Max(0, v) : prev`) onto one shape —
/// priorities #1 (minimize divergence) / #2 (shared Rust) / #4 (resolve drift).
/// See `docs/goal/behavior/value-formatting.md` § Tier cap validation.
pub fn parse_cap(input: &str) -> Option<i64> {
    input.trim().parse::<i64>().ok().map(|v| v.max(0))
}

/// Total page count for an admin list paginated by (`page_size`, `total` items),
/// ceil-divided and floored to `1` — an empty list still shows `"1 / 1"`, never
/// `"1 / 0"`. The `((total + page_size - 1) / page_size).max(1)` formula every
/// admin-users pagination surface hand-rolled byte-for-byte (web
/// `admin/users/+page.svelte`, windows `AdminUsersViewModel.TotalPages`, linux
/// `views/admin.rs::USERS_PAGE_SIZE`, tui `admin/users.rs`) — unifies onto one
/// source of truth (priority #1/#2/#4). Paired with [`current_page`] for the
/// `"{current} / {total}"` display. See `docs/goal/behavior/value-formatting.md`
/// § Pagination.
pub fn total_pages(total: i64, page_size: i64) -> i64 {
    ((total + page_size - 1) / page_size).max(1)
}

/// 1-based current page number from a 0-based `offset` into a list paginated by
/// `page_size`. Paired with [`total_pages`] — see its doc comment for the
/// per-app hand-rolls this unifies.
pub fn current_page(offset: i64, page_size: i64) -> i64 {
    offset / page_size + 1
}

/// The offset one page forward from `offset` (paginated by `page_size` over
/// `total` items), or `None` at the last page. The
/// `(offset + page_size < total).then_some(offset + page_size)` guard every
/// admin-users "next page" stepper hand-rolled byte-for-byte (tui
/// `admin/mod.rs::next_users_offset`, linux `views/admin.rs`'s next-click
/// handler) — the unfinished stepper half of the [`total_pages`]/[`current_page`]
/// display harvest, now unified onto one source of truth (priority #1/#2/#4).
/// See `docs/goal/behavior/value-formatting.md` § Pagination.
pub fn next_page_offset(offset: i64, total: i64, page_size: i64) -> Option<i64> {
    (offset + page_size < total).then_some(offset + page_size)
}

/// The offset one page back from `offset` (paginated by `page_size`), or `None`
/// at page 1. Floors to `0` rather than going negative, so a stale offset
/// recovers instead of underflowing. Paired with [`next_page_offset`] — see its
/// doc comment for the per-app hand-rolls this unifies.
pub fn prev_page_offset(offset: i64, page_size: i64) -> Option<i64> {
    (offset > 0).then(|| (offset - page_size).max(0))
}

/// Parse a user-entered admin-**mail** integer knob field (the outbound / alias /
/// spam / auth / submission / IMAP `admin-mail-*-input` numeric fields — e.g.
/// `permanent_failure_timeout_hours`, `delay_warning_at_hours`, `ndr_rate_limit_days`,
/// `exact_aliases_max`, `max_message_bytes`, `greylist_delay_secs`, `max_per_day`, …)
/// into a `u32`. Trims surrounding whitespace; a leading `+` is accepted (a valid
/// unsigned literal every app's native parser treats as valid). Returns `None`
/// for empty, non-numeric, negative, fractional, or out-of-`u32`-range input.
/// Like [`parse_port`] / [`parse_cap`] this is *input validation*, not formatting,
/// so it returns a plain `Option<u32>`, not a `LocalizedText`. Callers consume it as
/// `parse_count(text).unwrap_or(prev)` on a full-PUT mail-policy save, so an
/// empty/unparseable edit keeps the persisted value and **never silently zeroes a
/// knob**.
///
/// This unifies the per-app mail-knob parses that each hand-rolled the same
/// "non-negative integer, fall back to the persisted value" rule — linux
/// `entry.text().trim().parse::<u32>().unwrap_or(prev)` (`admin_mail.rs::parse_u32`),
/// windows `uint.TryParse(text?.Trim(), out v) ? v : prev` (`AdminMailPage.ParseUint`),
/// web `^\d+$` + `<= U32_MAX` (`admin/mail/+page.svelte::parseU32`), apple
/// `UInt32(s.trimmingCharacters(..)) ?? prev` (`AdminMailView.parseU32`), and android
/// `text.toUIntOrNull() ?: prev` (`AdminMailScreen`) — onto one source of truth
/// (priority #1 minimize divergence / #2 shared Rust / #4 resolve drift). The few
/// edge-case drifts converge here (web previously rejected a leading `+`; the others
/// accepted it — the canonical rule accepts it, matching `parse_cap`/`parse_port`).
/// See `docs/goal/behavior/value-formatting.md` § Mail-knob validation.
pub fn parse_count(input: &str) -> Option<u32> {
    input.trim().parse::<u32>().ok()
}

/// The `u64` sibling of [`parse_count`] for the one admin-mail knob whose range can
/// exceed `u32`: the IMAP per-mailbox storage ceiling
/// (`admin-mail-imap-storage-bytes-input` → `storage_bytes_default`). Same trim /
/// fallback-to-`prev` semantics — consumed as `parse_count_u64(text).unwrap_or(prev)`.
/// Unifies the per-app `parse_u64` / `ParseUlong` / `parseU64` / `toULongOrNull()`
/// parses onto one shape. See `docs/goal/behavior/value-formatting.md` § Mail-knob validation.
pub fn parse_count_u64(input: &str) -> Option<u64> {
    input.trim().parse::<u64>().ok()
}

/// The `i64` sibling of [`parse_count`] for the one per-alias knob whose wire column
/// is signed: the mail-alias `rate_limit_per_hour` override
/// (`mail-aliases-add-sheet-rate-per-hour-input` → `rate_limit_per_hour: Option<i64>`;
/// `mail-aliases.md` § Per-alias controls — null = unlimited, `0` = block all). The
/// column is signed `i64` only because SQLite/DAG-CBOR carry it that way; in *meaning*
/// it is a **non-negative** cap, so like every other count-family member this yields a
/// non-negative value — a negative input is meaningless and rejected to `None`. Trims
/// surrounding whitespace; returns `None` for empty, non-numeric, negative, fractional,
/// or out-of-`i64`-range input. The optional add-sheet field consumes it directly
/// (`None` = "no override / unlimited", not a fall-back-to-prev).
///
/// Unifies the per-app per-alias rate-cap parses onto one source of truth: web
/// `parseOptInt` (`Number(..)` + `Math.floor` — accepted a fractional value; rejected a
/// negative), windows `ParseOptI64` (`long.TryParse` — accepted a negative), apple
/// `Int64(String)` and android `toLongOrNull()` (both accepted a negative). The canonical
/// rule rejects **both** the fractional (web's `Math.floor` drift) and the negative (the
/// natives' incidental `long`-parse drift), matching web's deliberate `>= 0` guard and the
/// family's always-non-negative result — priorities #1 (minimize divergence) / #2 (shared
/// Rust) / #4 (resolve drift). The sibling `spam_threshold_override` (`Option<u32>`) needs
/// no new fn — it consumes the existing [`parse_count`]. See
/// `docs/goal/behavior/value-formatting.md` § Mail-knob validation.
pub fn parse_count_i64(input: &str) -> Option<i64> {
    input.trim().parse::<i64>().ok().filter(|&v| v >= 0)
}

/// The create-feed factor-weight editor's decimal multiplier (`feed-factor-weight-input`,
/// e.g. `"2.0"`) → the wire's **signed per-mille** `CompositionEntry.weight_permille`
/// (`docs/goal/architecture/content-moderation-and-ranking.md` § Composition, via
/// `fauna_feed::compose::FactorWeightInput`). `1.0` is the baseline weight the editors
/// pre-fill, so an **unparseable or non-finite** entry falls back to `1000` rather than
/// silently dropping the caller's add. A **negative** weight is a designed case, not an
/// error — a strong-negative factor sinks an item below any rendered page, which is how
/// filtering falls out of ordering (`docs/goal/ui/feed.md` § Frame reconciliation).
///
/// Rounds **half-away-from-zero**, matching the sibling
/// [`fauna_protocol::spam::probability_to_per_mille`], so every app maps a midpoint
/// to the same wire value. Unifies the per-app hand-rolls this lift replaces: linux
/// `text.parse::<f64>().unwrap_or(1.0)` then `(w * 1000.0).round()` (strict parse,
/// half-away-from-zero — the canonical rule), and web `Math.round(Number.parseFloat(..) * 1000)`
/// (**two** drifts: `parseFloat` is lenient, so `"2abc"` yielded `2000`; and JS `Math.round`
/// is half-**up**, so `-0.0025` yielded `-2` where linux yields `-3`). A C# hand-roll would
/// have added a third — `Math.Round`'s default is **banker's** rounding. Priorities
/// #1 (minimize divergence) / #2 (shared Rust) / #4 (resolve drift).
/// See `docs/goal/behavior/value-formatting.md` § Factor weight.
pub fn parse_weight_permille(input: &str) -> i64 {
    match input.trim().parse::<f64>() {
        Ok(w) if w.is_finite() => (w * 1000.0).round() as i64,
        _ => 1000,
    }
}

/// The inverse of [`parse_weight_permille`]: a wire `CompositionEntry.weight_permille`
/// → the create-feed factor chip's display multiplier (`"{name} × {weight}"`, e.g.
/// `feed-factor-weight-input`'s read-back). Rounds to 2 decimal places and strips
/// trailing zeros (and a bare trailing `.`), so a whole multiplier reads `"1"` /
/// `"2"`, never `"1.00"`.
///
/// Unifies three independent per-app hand-rolls: web
/// `(w / 1000).toFixed(2).replace(/\.?0+$/, '')`, windows `w / 1000.0` formatted
/// `"0.##"`, apple `Double(w) / 1000` formatted `"%.1f"`. Apple's fixed one-decimal
/// form is the odd one out — it loses precision `parse_weight_permille` itself
/// preserves (e.g. `1234` → `"1.2"`, silently dropping the last significant
/// digit) — so the 2-decimal, trailing-zero-stripped shape (web/windows) is the
/// richer convergent pattern kept here. Priorities #1 (minimize divergence) / #2
/// (shared Rust) / #4 (resolve drift, pick the richest pattern).
/// See `docs/goal/behavior/value-formatting.md` § Factor weight.
pub fn format_weight_permille(weight_permille: i64) -> String {
    let formatted = format!("{:.2}", weight_permille as f64 / 1000.0);
    let trimmed = formatted.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

/// The user-facing label for a backup-destination row: the `display_name` when
/// set (non-empty), else the destination URL's host (see [`url_host`]). One
/// source of truth so the six apps never drift on the blank-name fallback
/// (`docs/goal/behavior/backup-destinations.md` § State & data shape — "`None` ⇒ derive from the
/// destination domain"). Mirrors the per-app `destination_label` /
/// `DestinationLabel` / `label(for:)` glue this lift replaces.
pub fn backup_destination_label(display_name: Option<&str>, destination_nest_url: &str) -> String {
    match display_name {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => url_host(destination_nest_url),
    }
}

/// The `backup-destination-last-upload-time` row text, split for client-side
/// i18n: `label` is the outer i18n key — `backups.backup_destination_last_upload_never`
/// (complete as-is) when nothing has ever uploaded, else
/// `backups.backup_destination_last_upload`, whose `{when}` placeholder the
/// client fills by resolving `when` (a [`RelativeTimeDisplay`]; `Some` iff the
/// label needs it) through its own pipeline first. Two levels because a
/// [`LocalizedText`] arg is a flat string — the inner relative time must be
/// localized client-side before substitution, exactly the composition every
/// app hand-rolled. One source of truth for the never-vs-real decision,
/// the unix-**seconds** → epoch-ms conversion, and the `> 0` guard (linux/web
/// guarded zero timestamps; apple/android would have rendered the 1970 epoch —
/// the richest shape wins). See backups.md § Per-destination status read and
/// value-formatting.md § Backup destination status labels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct BackupLastUploadDisplay {
    pub label: LocalizedText,
    pub when: Option<RelativeTimeDisplay>,
}

/// Map a backup-destination status's `last_upload_time` (unix **seconds**;
/// `None` = no status read yet or nothing mirrored) to its row text — see
/// [`BackupLastUploadDisplay`] for the resolve contract.
pub fn backup_last_upload_label(
    last_upload_secs: Option<u64>,
    now_ms: i64,
) -> BackupLastUploadDisplay {
    match last_upload_secs {
        Some(secs) if secs > 0 => {
            let then_ms = i64::try_from(secs)
                .unwrap_or(i64::MAX)
                .saturating_mul(1_000);
            BackupLastUploadDisplay {
                label: LocalizedText::key("backups.backup_destination_last_upload"),
                when: Some(relative_time_display(now_ms, then_ms)),
            }
        }
        _ => BackupLastUploadDisplay {
            label: LocalizedText::key("backups.backup_destination_last_upload_never"),
            when: None,
        },
    }
}

/// [`backup_last_upload_label`] resolved to the finished
/// `backup-destination-last-upload-time` row text, against the caller's own
/// i18n lookup.
///
/// The two-level split stays exactly as value-formatting.md § Backup
/// destination status labels ratifies it — the outer label carries a `{when}`
/// slot, the inner relative time is localized first. What moved here is only
/// the *resolution* of that split, which was byte-identical in `fauna-linux`'s
/// `i18n::backup_last_upload` and `fauna-tui`'s `format::backup_last_upload`.
/// Native-only (`local-clock`) for [`relative_time_display_text`]'s reason; the
/// wasm face composes the same two levels in `$lib/value-format.ts` with the
/// browser's date formatter.
#[cfg(feature = "local-clock")]
pub fn backup_last_upload_text<F, S>(
    last_upload_secs: Option<u64>,
    now_ms: i64,
    lookup: F,
) -> String
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    let display = backup_last_upload_label(last_upload_secs, now_ms);
    let label = display.label.resolve(&lookup);
    match display.when {
        Some(when) => label.replace("{when}", &relative_time_display_text(&when, &lookup)),
        None => label,
    }
}

/// The `backup-destination-last-audit-time` row text, split for client-side
/// i18n exactly like [`BackupLastUploadDisplay`] — the same two-level shape
/// because the same composition problem applies (a [`LocalizedText`] arg is a
/// flat string, so the inner relative time must be localized client-side before
/// substitution).
///
/// It is a **separate type from the upload display on purpose**: the two rows
/// answer different questions and are advanced by different parties. The upload
/// row is the *source nest* reporting on its own work; this row is what the
/// *client's own* audit could independently confirm. Collapsing them into one
/// type would invite a client to render one where it meant the other — the
/// precise confusion the audit exists to prevent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct BackupLastAuditDisplay {
    pub label: LocalizedText,
    pub when: Option<RelativeTimeDisplay>,
}

/// Map a destination's last **passed** audit time (unix **seconds**; `None` =
/// never passed) to its `backup-destination-last-audit-time` row text — see
/// [`BackupLastAuditDisplay`] for the resolve contract.
///
/// "Never" is the honest reading of `None`, and it is deliberately *not* an
/// alerting state on its own: a destination enrolled ten minutes ago has never
/// passed an audit and is perfectly healthy. Escalating a long-standing "never"
/// is `AUDIT_OVERDUE`'s job (`fauna_client_backup::audit`), measured from
/// `added_at` — not this label's.
///
/// See `docs/goal/ui/backups.md` § Audit-alert surface and value-formatting.md
/// § Backup destination status labels.
pub fn backup_last_audit_label(
    last_passed_secs: Option<u64>,
    now_ms: i64,
) -> BackupLastAuditDisplay {
    match last_passed_secs {
        Some(secs) if secs > 0 => {
            let then_ms = i64::try_from(secs)
                .unwrap_or(i64::MAX)
                .saturating_mul(1_000);
            BackupLastAuditDisplay {
                label: LocalizedText::key("backups.backup_destination_last_audit"),
                when: Some(relative_time_display(now_ms, then_ms)),
            }
        }
        _ => BackupLastAuditDisplay {
            label: LocalizedText::key("backups.backup_destination_last_audit_never"),
            when: None,
        },
    }
}

/// Map a **client-device custodian's own** last-passed self-audit time (unix
/// **seconds**; `None` = never audited) to its
/// `backup-destination-last-audit-time` row text.
///
/// Why this is a separate door from [`backup_last_audit_label`] rather than a
/// flag on it: the two answer the same *question* from opposite sides of the
/// trust line. The owner-side loop verifies a destination **directly, trusting
/// neither the source nest nor the destination's self-report**
/// (`docs/goal/ui/backups.md` § Audit-alert surface); a client-device custodian
/// has no address for that loop to reach — inclusion-sampling a sleeping device
/// is structurally impossible (`backup-destinations.md` § Custodian contract,
/// question 4), so the custodian self-audits and its check-in feeds the nest
/// projection. Rendering that stamp through the owner-side label would let a
/// self-report wear the words of an independent verification, which is exactly
/// the confusion the audit surface exists to prevent. The wording therefore
/// names the provenance ("Self-checked"), and a caller cannot pick the wrong
/// claim by passing the wrong flag, because there is no flag.
///
/// A custodian that has never self-audited renders **absence**, never a
/// verdict — the same refusal the nest makes when it declines to invent one for
/// a row that has not reported.
pub fn backup_self_audit_label(
    last_passed_secs: Option<u64>,
    now_ms: i64,
) -> BackupLastAuditDisplay {
    match last_passed_secs {
        Some(secs) if secs > 0 => {
            let then_ms = i64::try_from(secs)
                .unwrap_or(i64::MAX)
                .saturating_mul(1_000);
            BackupLastAuditDisplay {
                label: LocalizedText::key("backups.backup_destination_last_self_audit"),
                when: Some(relative_time_display(now_ms, then_ms)),
            }
        }
        _ => BackupLastAuditDisplay {
            label: LocalizedText::key("backups.backup_destination_last_self_audit_never"),
            when: None,
        },
    }
}

/// [`backup_self_audit_label`] resolved to the finished row text — the
/// custodian twin of [`backup_last_audit_text`], separate for the same reason
/// the two label doors are.
#[cfg(feature = "local-clock")]
pub fn backup_self_audit_text<F, S>(last_passed_secs: Option<u64>, now_ms: i64, lookup: F) -> String
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    let display = backup_self_audit_label(last_passed_secs, now_ms);
    let label = display.label.resolve(&lookup);
    match display.when {
        Some(when) => label.replace("{when}", &relative_time_display_text(&when, &lookup)),
        None => label,
    }
}

/// Whether a custodian's reported `audit_state` is a state the Backups page must
/// be **loud** about — the one place that decision is made, so no app can drift
/// into alerting on a custodian that has simply not audited yet.
///
/// Only [`crate::data::AUDIT_STATE_FAILED`] is loud. Absence is **not**: every
/// custodian shipped before the carrier landed sends no verdict at all, and a
/// fleet-wide false data-loss alarm is the loudest possible way to get this
/// wrong (`backup-destinations.md` § Third destination kind). An unrecognised
/// value a newer client wrote is also quiet — this client cannot know whether it
/// names a failure, and inventing one is the same false alarm.
pub fn backup_self_audit_is_alerting(audit_state: Option<&str>) -> bool {
    audit_state == Some(crate::data::AUDIT_STATE_FAILED)
}

/// [`backup_last_audit_label`] resolved to the finished
/// `backup-destination-last-audit-time` row text — the audit twin of
/// [`backup_last_upload_text`], and a separate door for the same reason the two
/// display types are separate: the rows assert different things, and one
/// resolver taking a flag would let a caller render the wrong claim.
#[cfg(feature = "local-clock")]
pub fn backup_last_audit_text<F, S>(last_passed_secs: Option<u64>, now_ms: i64, lookup: F) -> String
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    let display = backup_last_audit_label(last_passed_secs, now_ms);
    let label = display.label.resolve(&lookup);
    match display.when {
        Some(when) => label.replace("{when}", &relative_time_display_text(&when, &lookup)),
        None => label,
    }
}

/// Why a `backup-audit-alert` banner is showing.
///
/// Three of its arms are the plain-data mirror of the three alerting
/// `AuditVerdict`s — the owner-side loop's own findings. The fifth,
/// [`Self::SourceRegressed`], is a recovery notice rather than a finding
/// against the destination. The fourth,
/// [`Self::SelfReported`], has **no** verdict behind it: it carries a
/// client-device custodian's own failing self-audit, which reaches the page on
/// the status row instead (`backup-destinations.md` § Custodian contract,
/// question 4). It is a banner reason rather than a verdict because the
/// owner-side loop can never produce it: a custodian has no address to sample.
///
/// It lives here rather than in `fauna-client-backup` because that crate
/// *depends on* this one, so the format layer cannot name `AuditVerdict`. The
/// single map between them is `AuditVerdict::alert_reason()`, and that verdict
/// enum defines `is_alerting()` through it — so a fourth alerting verdict cannot
/// be added without also giving it a banner, which is the failure mode this
/// arrangement is designed to make impossible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum BackupAuditAlertReason {
    /// The destination's backed-up high-water lags what the client itself holds.
    Freshness { lag_secs: i64 },
    /// A sampled record was missing, or present but unopenable under the
    /// owner's derived `NestBackupKey`.
    Inclusion { missing: u32, sampled: u32 },
    /// No audit pass has succeeded for longer than `AUDIT_OVERDUE`.
    Overdue { since_secs: i64 },
    /// A client-device custodian **reported its own copy as failing** at its
    /// last check-in (`AUDIT_STATE_FAILED`). Loud because a reported failure is
    /// the only failure signal that exists for a kind the owner cannot sample —
    /// and because withholding it silences the row, which reads as the
    /// sleeping-device case: the wrong alarm, thirty days late.
    SelfReported,
    /// The owner's own nest **went backwards** (restored from an older copy of
    /// its data) and this destination still holds what it lost — the recovery
    /// notice of an accepted source regression (`backup-restore.md` §
    /// Background Tasks → *Implementation status (audit loop)*, the
    /// accepted-regression bullet). `left_secs` is how long until the first of
    /// what it holds is reclaimed; absent when no deadline exists — the copy
    /// holds it as ordinary live rows, kept until recovered. Like
    /// `SelfReported` it has no verdict behind it: the destination did nothing
    /// wrong, so the audit still passes; `DestinationAuditRecord::alert_reasons`
    /// raises it from the client-local record while that record stands, and it
    /// stops by itself.
    SourceRegressed { left_secs: Option<i64> },
}

/// The `backup-audit-alert` banner text for one failing destination: a complete
/// [`LocalizedText`] naming **both** the destination and the reason, because the
/// banner is indexed (one per failing destination) and a bare "backup problem"
/// would not say *which* one.
///
/// The lag/overdue durations are rendered as whole **days** (floored, minimum 1)
/// rather than a relative timestamp: both thresholds are multi-day by
/// construction (`FRESHNESS_SLACK` = 48 h, `AUDIT_OVERDUE` = 7 d), and "3 days
/// behind" is the sentence a user can act on, where "last Tuesday" is not.
///
/// See `docs/goal/ui/backups.md` § Audit-alert surface.
pub fn backup_audit_alert_label(
    reason: BackupAuditAlertReason,
    destination_label: &str,
) -> LocalizedText {
    let mut args = std::collections::HashMap::new();
    args.insert("destination".to_string(), destination_label.to_string());
    let key = match reason {
        BackupAuditAlertReason::Freshness { lag_secs } => {
            args.insert("days".to_string(), whole_days(lag_secs).to_string());
            "backups.backup_audit_alert_freshness"
        }
        BackupAuditAlertReason::Inclusion { missing, sampled } => {
            args.insert("missing".to_string(), missing.to_string());
            args.insert("sampled".to_string(), sampled.to_string());
            "backups.backup_audit_alert_inclusion"
        }
        BackupAuditAlertReason::Overdue { since_secs } => {
            args.insert("days".to_string(), whole_days(since_secs).to_string());
            "backups.backup_audit_alert_overdue"
        }
        BackupAuditAlertReason::SelfReported => "backups.backup_audit_alert_self_reported",
        BackupAuditAlertReason::SourceRegressed {
            left_secs: Some(left_secs),
        } => {
            args.insert("days".to_string(), whole_days(left_secs).to_string());
            "backups.backup_audit_alert_source_regressed"
        }
        BackupAuditAlertReason::SourceRegressed { left_secs: None } => {
            "backups.backup_audit_alert_source_regressed_until_recovered"
        }
    };
    LocalizedText {
        key: key.to_string(),
        args,
    }
}

/// Whole days in `secs`, floored, never below 1 — a sub-day remainder still
/// happened, and "0 days behind" reads as "not behind".
fn whole_days(secs: i64) -> i64 {
    (secs / (24 * 60 * 60)).max(1)
}

/// The `backup-destination-kind-badge` text for one destination row.
///
/// The badge exists because a client custodian "never silently satisfies *you
/// have an off-site backup*" (`docs/goal/behavior/backup-destinations.md` § Third destination kind
/// → *Durability + labeling*) — so the distinction has to be **visible per row**,
/// not inferred from a URL column that a client-device row does not have.
///
/// Takes the raw `kind` discriminator rather than
/// [`crate::data::DestinationKind`] because that projection borrows from the row
/// and carries per-kind payload this label never reads; the badge answers only
/// "which kind is this". An unrecognised kind is rendered with the raw string
/// interpolated rather than collapsed into a generic word: a newer client's row
/// is precisely the case where the user needs to see *what* their older build
/// cannot drive (the `Inert` arm's whole reason for existing).
pub fn backup_destination_kind_label(kind: &str) -> LocalizedText {
    match kind {
        crate::data::DESTINATION_KIND_NEST => {
            LocalizedText::key("backups.backup_destination_kind_nest")
        }
        crate::data::DESTINATION_KIND_CLIENT_DEVICE => {
            LocalizedText::key("backups.backup_destination_kind_client_device")
        }
        other => LocalizedText::key_arg(
            "backups.backup_destination_kind_unknown",
            "kind",
            other.to_string(),
        ),
    }
}

/// One arm of `backup-destination-kind-select`: the wire discriminator the app
/// records, and the text it paints.
///
/// Crosses both boundaries directly (like [`BackupUsageDisplay`] below) rather
/// than through an `Ffi*` mirror: `fauna_core` carries its own `uniffi` feature,
/// so the mirror pattern the option catalogs in `fauna_client_admin` /
/// `fauna_client_feed` need (`fauna_ffi::FfiRegistrationModeOption`) does not
/// apply here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct BackupDestinationKindOption {
    /// The `BackupDestination.kind` value this option writes — never a per-app
    /// string literal.
    pub value: String,
    /// The option text, which is deliberately the *same* [`LocalizedText`] the
    /// row's badge will carry.
    pub label: LocalizedText,
}

/// The `backup-destination-kind-select` option catalog — the implemented kinds
/// in paint order (`docs/goal/behavior/backup-destinations.md` § Third destination kind).
///
/// Shared for the reason the [`crate::data::every_destination_is_a_client_device`]
/// predicate is (priority #2), plus one specific to a select: **the option a user
/// picks and the badge they get back on the resulting row must be the same
/// text**, and seven apps each pairing a hand-written option list against
/// [`backup_destination_kind_label`] is seven chances for those to drift. Mirrors
/// `fauna_client_admin::registration_mode_options`, the same catalog shape linux's
/// registration-mode dropdown already consumes.
///
/// **Nest is first, and that is the default a shell lands on** — it is the kind
/// that actually satisfies "off-site", which is the whole premise of
/// § *Durability + labeling*'s standing warning.
///
/// The ratified-but-deferred S3 kind (§ Second destination kind) is **absent**
/// rather than present-and-disabled: an option that cannot be chosen teaches the
/// user nothing and would need its own "not yet" copy. It joins here, once, when
/// its design pass lands.
pub fn backup_destination_kind_options() -> Vec<BackupDestinationKindOption> {
    [
        crate::data::DESTINATION_KIND_NEST,
        crate::data::DESTINATION_KIND_CLIENT_DEVICE,
    ]
    .into_iter()
    .map(|value| BackupDestinationKindOption {
        value: value.to_string(),
        label: backup_destination_kind_label(value),
    })
    .collect()
}

/// The `backup-destination-usage` row text — client-device rows only: held bytes
/// against the user-set cap.
///
/// Split for client-side i18n exactly like [`BackupLastUploadDisplay`], and for
/// the same reason: a [`LocalizedText`] arg is a flat string, so the two inner
/// [`byte_size`] texts must be localized client-side before substitution.
///
/// **Cap-reached is read from `cap_state`, never inferred from `held >= cap`.**
/// The pull pass reports `CAP_STATE_REACHED` for a pass that stopped at its cap
/// even though it ends *below* the cap (a segment larger than the remaining
/// headroom stops the pass without filling it) — so a client re-deriving the
/// verdict from the two numbers would render "healthy, with room to spare" for a
/// backup that has silently stopped advancing. That inversion is
/// mutation-pinned in `fauna-sync-engine`'s pull tests; this label is the other
/// half of the same guarantee.
///
/// See `docs/goal/behavior/backup-destinations.md` § Third destination kind and
/// `docs/goal/behavior/value-formatting.md` § Backup destination status labels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct BackupUsageDisplay {
    /// The outer key. Carries `{held}` and/or `{cap}` placeholders the client
    /// fills from the resolved fields below.
    pub label: LocalizedText,
    /// The held-bytes text to resolve and substitute for `{held}`; `Some` iff
    /// `label` names it.
    pub held: Option<LocalizedText>,
    /// The cap text to resolve and substitute for `{cap}`; `Some` iff `label`
    /// names it.
    pub cap: Option<LocalizedText>,
}

/// Map a client-device destination's held bytes + cap + `cap_state` to its
/// `backup-destination-usage` row text — see [`BackupUsageDisplay`] for the
/// resolve contract and the cap-state rule.
///
/// `held_bytes: None` is "this custodian has never checked in", which reads as
/// *nothing held yet* rather than *0 bytes held*: the two are different claims,
/// and only the second one asserts the device actually reported.
pub fn backup_usage_label(
    held_bytes: Option<u64>,
    capacity_cap_bytes: Option<u64>,
    cap_state: Option<&str>,
) -> BackupUsageDisplay {
    let Some(held) = held_bytes else {
        return BackupUsageDisplay {
            label: LocalizedText::key("backups.backup_destination_usage_unknown"),
            held: None,
            cap: None,
        };
    };
    let held_text = byte_size(held);
    match capacity_cap_bytes {
        Some(cap) => {
            let reached = cap_state == Some(crate::data::CAP_STATE_REACHED);
            BackupUsageDisplay {
                label: LocalizedText::key(if reached {
                    "backups.backup_destination_usage_cap_reached"
                } else {
                    "backups.backup_destination_usage"
                }),
                held: Some(held_text),
                cap: Some(byte_size(cap)),
            }
        }
        // Uncapped is a real configuration (`CustodianEnrollment::capacity_cap_bytes`
        // documents `None` as "fill the disk"), so it gets its own sentence
        // rather than an "of —" with an empty cap.
        None => BackupUsageDisplay {
            label: LocalizedText::key("backups.backup_destination_usage_uncapped"),
            held: Some(held_text),
            cap: None,
        },
    }
}

/// [`backup_usage_label`] resolved to the finished `backup-destination-usage`
/// row text — two-level like [`backup_last_upload_text`], but with **two**
/// inner texts rather than one: both byte sizes are themselves
/// [`LocalizedText`] (unit key + value), so each resolves before substitution.
///
/// Each slot is filled only when the shared decision named it, so an arm whose
/// label has no `{cap}` cannot grow one — the cap-reached decision stays
/// entirely inside [`backup_usage_label`], which reads `cap_state` and never
/// infers it from `held >= cap`.
///
/// Unlike the two `{when}` doors this one needs no local clock of its own, but
/// it carries the same gate: it exists to spare the native apps a hand-rolled
/// resolve, and gating it with its siblings keeps the whole family one story.
#[cfg(feature = "local-clock")]
pub fn backup_usage_text<F, S>(
    held_bytes: Option<u64>,
    capacity_cap_bytes: Option<u64>,
    cap_state: Option<&str>,
    lookup: F,
) -> String
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    let display = backup_usage_label(held_bytes, capacity_cap_bytes, cap_state);
    let mut text = display.label.resolve(&lookup);
    if let Some(held) = display.held {
        text = text.replace("{held}", &held.resolve(&lookup));
    }
    if let Some(cap) = display.cap {
        text = text.replace("{cap}", &cap.resolve(&lookup));
    }
    text
}

/// [`orphaned_store_label`]'s two-level result — the row sentence plus the
/// byte size that fills its `{held}` slot, each a [`LocalizedText`] so the unit
/// resolves in the reader's language before substitution.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct OrphanedStoreDisplay {
    /// The row sentence, carrying `{held}`.
    pub label: LocalizedText,
    /// The size that fills it.
    pub held: LocalizedText,
}

/// The `backup-orphaned-store-row` text — what this device is still holding
/// with no destination row to justify it (`docs/goal/ui/backups.md` § Manage
/// backup destinations → *Reclaim this device's copy*).
///
/// Shared for the reason the whole client-device label family is: the row is
/// the only place a user is told that deleting these bytes costs them their
/// standalone restore, and seven apps writing that sentence is seven chances to
/// undersell it. The **rule** that decides whether the row paints at all is
/// `crate::data::custodian_store_is_orphaned`, not this function — a formatter
/// that also decided visibility would be a policy answer wearing a label's
/// clothes.
pub fn orphaned_store_label(held_bytes: u64) -> OrphanedStoreDisplay {
    OrphanedStoreDisplay {
        label: LocalizedText::key("backups.backup_orphaned_store_row"),
        held: byte_size(held_bytes),
    }
}

/// [`orphaned_store_label`] resolved to the finished row text — the same
/// two-level resolve [`backup_usage_text`] performs, gated with its siblings so
/// the family stays one story.
#[cfg(feature = "local-clock")]
pub fn orphaned_store_text<F, S>(held_bytes: u64, lookup: F) -> String
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    let display = orphaned_store_label(held_bytes);
    display
        .label
        .resolve(&lookup)
        .replace("{held}", &display.held.resolve(&lookup))
}

/// The `backup-destination-backlog-count` row text ("{count} queued"): a
/// status's `backlog_count`, with the absent-status baseline of 0 every app
/// applied by hand. Returns a complete [`LocalizedText`] — no client-side
/// composition. See value-formatting.md § Backup destination status labels.
pub fn backup_backlog_label(backlog_count: Option<u32>) -> LocalizedText {
    LocalizedText::key_arg(
        "backups.backup_destination_backlog",
        "count",
        backlog_count.unwrap_or(0).to_string(),
    )
}

/// The contact-roster filter predicate (`contacts-search-field`): does a contact
/// row match the typed query? A case-insensitive substring test of the trimmed
/// query against the contact's searchable fields — `handle`, `domain`, and the hex
/// `actor_id` (the richest existing per-app shape). An empty or whitespace-only
/// query matches every row. `handle`/`domain` are `None` for a federated peer (the
/// nest holds no cached federated Profile), so those simply don't contribute a match.
///
/// The filter is **local-only** — it narrows the already-loaded accepted-contacts
/// roster, never a nest query (contrast the federated `classify_recipient` lookup
/// path). Shared so all seven apps filter through one predicate over uniform
/// `handle + domain + actor-id` data, resolving today's field-drift (web matched
/// actor-id only, linux/windows handle+actor-id, android handle+domain+actor-id,
/// iOS/macOS had no filter). See contacts.md § Where logic lives → Contact roster
/// filter; the row data comes from the enriched `fauna.contacts.list`
/// (`ContactItem { …, handle, domain }`).
///
/// The viewer's private overlay adds two more searchable fields
/// (contacts.md § The private overlay → *Labels and the roster filter*): the
/// `nickname`, and each live label in `labels` — typing a label narrows the
/// roster to the people carrying it. Notes are deliberately NOT matched: a row
/// that matches for a reason it does not show is a confusing filter. A caller
/// with no overlay projection passes `None` / `&[]`.
pub fn contact_matches_filter(
    query: &str,
    handle: Option<&str>,
    domain: Option<&str>,
    actor_id: &str,
    nickname: Option<&str>,
    labels: &[String],
) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    let matches = |field: &str| field.to_lowercase().contains(&q);
    handle.is_some_and(matches)
        || domain.is_some_and(matches)
        || matches(actor_id)
        || nickname.is_some_and(matches)
        || labels.iter().any(|l| matches(l))
}

/// Map a contact relationship status to its human label, as a [`LocalizedText`]
/// each app resolves through its own i18n pipeline. The canonical vocabulary
/// (contacts.md § Encryption at rest; the `crate::data::ContactStatus` enum) —
/// `pending` / `accepted` / `confirmed` / `blocked` — carries its `common.*`
/// key; any unknown status (e.g. linux's outbound-knock `sent`, or a future
/// status) falls back to its capitalized form rendered verbatim (no i18n entry,
/// so `resolve` returns the key as-is), preserving the richest prior per-app
/// behavior and beating windows' lossy `_ => "Unknown"`. Shared across every
/// app so the status → label contract can't drift per-app; previously
/// hand-rolled divergently (web/linux raw-lowercase, android/apple raw-
/// capitalized, windows i18n-keyed but missing `confirmed` → "Unknown"). The
/// status **icon/color** stays an idiomatic per-app render (contacts.md), not
/// part of this lift — the same split as [`crate::ical::rsvp_status_label`].
pub fn contact_status_label(status: &str) -> LocalizedText {
    match status {
        "pending" => LocalizedText::key("common.pending"),
        "accepted" => LocalizedText::key("common.accepted"),
        "confirmed" => LocalizedText::key("common.confirmed"),
        "blocked" => LocalizedText::key("common.blocked"),
        "" => LocalizedText::default(),
        other => {
            // Unknown status → capitalize the first char and render verbatim
            // (no i18n entry matches, so `resolve` returns the key as-is).
            let mut chars = other.chars();
            let capitalized = match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            };
            LocalizedText::key(capitalized)
        }
    }
}

/// Map a conversation thread's raw `label` to its display string, as a
/// [`LocalizedText`] each app resolves through its own i18n pipeline. A thread
/// whose label is empty or whitespace-only carries the canonical
/// `conversations.detail.no_subject` key (`"(no subject)"`); any non-empty label
/// rides verbatim as its own key, so `resolve` (which falls back to the key when no
/// i18n entry matches) returns the raw name unchanged — the same passthrough trick
/// [`contact_status_label`] uses for an unknown status.
///
/// Shared across every app so the empty-label fallback can't drift per-app.
/// Previously hand-rolled divergently: windows/apple hardcoded the untranslated
/// literal `"(no label)"` (priority #1 violation), android used the i18n
/// `conversations.detail.no_subject` (`"(no subject)"`), and linux/web applied **no**
/// fallback (an empty label rendered blank — a latent bug). This converges all six on
/// android's richest existing pattern (priority #1/#2/#4). The raw `label` stays the
/// value used for thread-list filter/sort and the rename field — only the *display*
/// derivation lives here. See conversations.md § Where logic lives.
pub fn thread_label_display(label: &str) -> LocalizedText {
    if label.trim().is_empty() {
        LocalizedText::key("conversations.detail.no_subject")
    } else {
        LocalizedText::key(label.to_string())
    }
}

/// Map a `media-sort-select` UI value (`"name"` / `"size"` / `"date"` — the same
/// wire form as `fauna_client_media::MediaSortKey::as_select_value`) to its
/// `media.sort_*` label, as a [`LocalizedText`] each app resolves through its
/// own i18n pipeline. An unrecognized value falls back to `sort_name` — the
/// enum's own `#[default]` variant — rather than a blank or raw-value render.
///
/// Takes the raw wire value as `&str` (not `MediaSortKey`) for the same reason
/// [`cert_status_label`] does — `fauna_core` does not depend on
/// `fauna-client-media`.
///
/// Shared so the value → key decision can't drift per-app. All six apps
/// offering this select currently hand-roll the identical three-case map
/// (linux `build_sort_dropdown`, tui `sort_label`, web `sortLabel`, apple
/// `MediaExplorerContent`, android `MediaScreen`, windows `MediaPage`) — and
/// had already drifted on the unrecognized-value fallback (linux rendered
/// blank, web echoed the raw wire value); this converges every app on the
/// richest existing behavior (tui's fallback to Name). See media.md § Layout
/// & flow.
pub fn media_sort_label(value: &str) -> LocalizedText {
    match value {
        "size" => LocalizedText::key("media.sort_size"),
        "date" => LocalizedText::key("media.sort_date"),
        _ => LocalizedText::key("media.sort_name"),
    }
}

/// Map a `share-link-expiry-select` value (one of
/// `fauna_client_share::EXPIRY_OPTIONS`: `"1d"` / `"7d"` / `"30d"` / `"1y"`)
/// to its `share_link.expiry_*` label, as a [`LocalizedText`] each app resolves
/// through its own i18n pipeline; `None` for a value outside the list, which
/// the app paints raw (the machine never stores one — `set_share_expiry`
/// refuses it).
///
/// Takes `&str` for the [`media_sort_label`] reason (`fauna_core` does not
/// depend on `fauna-client-share`). Shared so the value → key map is written
/// once rather than per app, as tui and linux first had it
/// (`share-links.md` § Expiry).
pub fn share_link_expiry_label(value: &str) -> Option<LocalizedText> {
    let key = match value {
        "1d" => "share_link.expiry_1d",
        "7d" => "share_link.expiry_7d",
        "30d" => "share_link.expiry_30d",
        "1y" => "share_link.expiry_1y",
        _ => return None,
    };
    Some(LocalizedText::key(key))
}

/// Map a share-link list row's stable state (`fauna_client_share::LinkState`
/// `as_str`: `"active"` / `"expired"` / `"revoked"`) to its
/// `share_link.state_*` label — the `share-link-item-state` text; the stable
/// value stays the element's `state` attribute. `None` for an unknown state,
/// painted raw. Same shape and reason as [`share_link_expiry_label`]
/// (`share-links.md` § Flows → List).
pub fn share_link_state_label(state: &str) -> Option<LocalizedText> {
    let key = match state {
        "active" => "share_link.state_active",
        "expired" => "share_link.state_expired",
        "revoked" => "share_link.state_revoked",
        _ => return None,
    };
    Some(LocalizedText::key(key))
}

/// Map the `media-sort-direction` `descending` flag to its `media.sort_*`
/// label, as a [`LocalizedText`] each app resolves through its own i18n
/// pipeline.
///
/// Shared so the bool → key decision can't drift per-app — previously
/// hand-rolled identically wherever the direction toggle exists (tui
/// `sort_direction_label`, apple `sortDirectionPicker`). See media.md §
/// Layout & flow.
pub fn media_sort_direction_label(descending: bool) -> LocalizedText {
    if descending {
        LocalizedText::key("media.sort_descending")
    } else {
        LocalizedText::key("media.sort_ascending")
    }
}

/// Map host-OS maintenance state (the `os_*` fields on `fauna.setup.status`) to the
/// `admin-nest` `nest-os-maintenance-status` line, as a [`LocalizedText`] each app
/// resolves through its own i18n pipeline. Priority: a **pending reboot** is the
/// headline (`os_restart_pending`, "Restart pending — will restart automatically when
/// idle"); else **pending security updates** (`os_updates_pending`, "Security updates
/// pending" — the raw count renders separately in `nest-os-updates-count` from
/// `os_security_updates_pending`); else **up to date** (`os_up_to_date`). A nest with
/// no host maintenance channel (dev/desktop/older nest) reports the serde defaults
/// (0/false), so this returns `os_up_to_date` — no false alarm on version skew.
///
/// Shared across all seven apps (priority #2) so the state→line decision can't drift.
/// Authority: `installers/vps.md` § Host OS Maintenance § 4. Strings live in en.yaml
/// (`admin.nest_page.os_{up_to_date,updates_pending,restart_pending}`).
pub fn os_maintenance_status_label(
    security_updates_pending: u32,
    reboot_pending: bool,
) -> LocalizedText {
    if reboot_pending {
        LocalizedText::key("admin.nest_page.os_restart_pending")
    } else if security_updates_pending > 0 {
        LocalizedText::key("admin.nest_page.os_updates_pending")
    } else {
        LocalizedText::key("admin.nest_page.os_up_to_date")
    }
}

/// The i18n key every mail-health label falls back to for a token this build
/// does not know ("Needs attention").
const MAIL_HEALTH_UNKNOWN_KEY: &str = "admin.mail_page.health_state_unknown";

/// Map the `fauna.bridges.mail_health` reply's categorical `state` to the
/// `admin-mail-health-status` line (and the dashboard's Mail stat card), as a
/// [`LocalizedText`] each app resolves through its own i18n pipeline. The wire
/// `state` is an **open** string enum decided nest-side by the shared fold
/// (`fauna_mail::health`); a token this build does not know renders the generic
/// "needs attention" key, so a newer nest never breaks an older app.
///
/// Shared across all seven apps (priority #2) — the `os_maintenance_status_label`
/// pattern. Authority: `mail-deliverability.md` § The mail health readout. Strings
/// live in en.yaml (`admin.mail_page.health_state_*`).
pub fn mail_health_state_label(state: &str) -> LocalizedText {
    LocalizedText::key(match state {
        "off" => "admin.mail_page.health_state_off",
        "bridge_down" => "admin.mail_page.health_state_bridge_down",
        "blocklisted" => "admin.mail_page.health_state_blocklisted",
        "queue_stalled" => "admin.mail_page.health_state_queue_stalled",
        "records_failing" => "admin.mail_page.health_state_records_failing",
        "warming_up" => "admin.mail_page.health_state_warming_up",
        "delivering" => "admin.mail_page.health_state_delivering",
        _ => MAIL_HEALTH_UNKNOWN_KEY,
    })
}

/// Map one `admin-mail-health-check` row's `state` (`pass` / `warn` / `fail` /
/// `info`) to its `-state` label, as a [`LocalizedText`]. An unknown token renders
/// the same generic "needs attention" key as [`mail_health_state_label`].
/// Authority: `mail-deliverability.md` § The mail health readout.
pub fn mail_health_check_state_label(state: &str) -> LocalizedText {
    LocalizedText::key(match state {
        "pass" => "admin.mail_page.health_check_pass",
        "warn" => "admin.mail_page.health_check_warn",
        "fail" => "admin.mail_page.health_check_fail",
        "info" => "admin.mail_page.health_check_info",
        _ => MAIL_HEALTH_UNKNOWN_KEY,
    })
}

/// One mail-health heartbeat stamp (`last_outbound_delivered_at` /
/// `last_inbound_accepted_at`, Unix **seconds**; `None` = never) as the shared
/// relative-time display: `admin.mail_page.health_never` ("Never") for `None`,
/// else the ordinary relative bucket — or, past a week, the absolute date the
/// caller renders with its own date formatter. It is the text of the heartbeat
/// rows (indices 5 and 6, whose `detail` the fold leaves empty on purpose) and of
/// the two facts on the status line. The stamps are facts, never a state input.
/// Authority: `mail-deliverability.md` § The mail health readout.
pub fn mail_health_heartbeat_label(at_secs: Option<i64>, now_ms: i64) -> RelativeTimeDisplay {
    match at_secs {
        Some(secs) => relative_time_display(now_ms, secs.saturating_mul(1_000)),
        None => RelativeTimeDisplay {
            localized: Some(LocalizedText::key("admin.mail_page.health_never")),
            absolute_epoch_ms: None,
        },
    }
}

/// [`mail_health_heartbeat_label`] resolved to finished text against the
/// caller's own i18n lookup — the native Rust apps' door (`local-clock`, for
/// [`relative_time_display_text`]'s reason).
#[cfg(feature = "local-clock")]
pub fn mail_health_heartbeat_text<F, S>(at_secs: Option<i64>, now_ms: i64, lookup: F) -> String
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    relative_time_display_text(&mail_health_heartbeat_label(at_secs, now_ms), lookup)
}

/// The finished `admin-mail-health-status` line: the shared label of `state`
/// ([`mail_health_state_label`]) followed by the two heartbeat facts, composed
/// through the one `admin.mail_page.health_status_line` template so every app
/// paints the same sentence. Native-only (`local-clock`), like
/// [`mail_health_heartbeat_text`].
#[cfg(feature = "local-clock")]
pub fn mail_health_status_text<F, S>(
    state: &str,
    last_delivered_at: Option<i64>,
    last_received_at: Option<i64>,
    now_ms: i64,
    lookup: F,
) -> String
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    let template = LocalizedText::key("admin.mail_page.health_status_line").resolve(&lookup);
    template
        .replace("{state}", &mail_health_state_label(state).resolve(&lookup))
        .replace(
            "{delivered}",
            &mail_health_heartbeat_text(last_delivered_at, now_ms, &lookup),
        )
        .replace(
            "{received}",
            &mail_health_heartbeat_text(last_received_at, now_ms, &lookup),
        )
}

/// The `profile-block-button` toggle label: `profile.unblock` (`"Unblock"`) when the
/// viewed actor's contact edge is already `blocked`, else `profile.block` (`"Block"`),
/// as a [`LocalizedText`] each app resolves through its own i18n pipeline. The
/// button's `contact_status`-driven state is derived by [`contact_row_blocks_actor`]
/// folded over the roster; this maps that `bool` to its label.
///
/// Shared so the toggle wording can't drift per-app — previously hand-rolled
/// identically across all five apps that surface the button (web/linux/windows/
/// apple/android), a *preventative* lift (priority #2/#4). The
/// button's *style* (linux's `destructive-action` on "Block") stays an idiomatic
/// per-app render, the same split as the status icon/color in [`contact_status_label`].
/// The block/unblock edge transition itself lives in the shared `fauna-client-contacts`
/// knocks client (`fauna.knocks.{block,unblock}`); see profile.md § Element table and
/// contacts.md § Where logic lives → Unblock.
pub fn contact_toggle_block_label(is_blocked: bool) -> LocalizedText {
    if is_blocked {
        LocalizedText::key("profile.unblock")
    } else {
        LocalizedText::key("profile.block")
    }
}

/// The `profile-follow-button` label: `profile.following` (`"Following"`) when the
/// viewer already follows the viewed actor, else `profile.follow` (`"Follow"`), as a
/// [`LocalizedText`] each app resolves through its own i18n pipeline. Follow *is*
/// subscribing to the free "followers" tier (profile.md § Where logic lives → *Follow /
/// unfollow*); this maps the resulting `is_following` bool to its label — the same
/// bool→2-key shape as [`contact_toggle_block_label`] / [`device_status_label`].
///
/// Shared so the toggle wording can't drift per-app — previously hand-rolled on
/// web/android (ternary over the same two keys), linux/windows (initial "Follow" +
/// an on-success flip to "Following"), while apple rendered a bare always-"Follow"
/// (no Following state at all — the drift this lift retires). The `is_following`
/// *derivation* (subscription status / optimistic flip) and the `FOLLOWERS_TIER`
/// constant stay per-app by ratified decision (`fauna_core::subscription`,
/// 2026-07-08) — only the bool→label map is shared. Button *style* stays an
/// idiomatic per-app render, the same split as [`contact_toggle_block_label`].
pub fn follow_toggle_label(is_following: bool) -> LocalizedText {
    if is_following {
        LocalizedText::key("profile.following")
    } else {
        LocalizedText::key("profile.follow")
    }
}

/// One option in a guardian policy editor's select (`family-safety.md` § Guardian
/// policy pillar 1) — the canonical wire value plus the i18n key the client
/// resolves. Mirrors `fauna_folders_machine::ConflictPolicyOption`'s shape:
/// shared Rust owns the option *set*, the client owns the widget.
///
/// `value` is a `String`, not a typed enum, on purpose — the same convention
/// `FfiReachPolicy.feed_sources` follows: closed-enum FFI *fields* cross as
/// strings. This is an option **catalog** (a UI picker descriptor), not a wire
/// field, which is why it may be a `uniffi::Record` while the knob it describes
/// stays a string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ReachPolicyOption {
    /// The canonical wire/DB value ([`UnknownSenderMail::as_str`] /
    /// [`FeedSources::as_str`]) — what the select writes to
    /// `guardian_policies.*` and what the cross-app `select(id, value)` e2e
    /// contract drives.
    pub value: String,
    pub label: LocalizedText,
}

/// One rendered line of the read-only policy summary the *supervised* side sees
/// (`family-policy-summary`). Structured rather than a pre-joined string so each
/// app keeps its own line layout — linux joins with `\n`, the mobile clients
/// stack rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PolicySummaryLine {
    /// The knob's name (`family.policy_*_label`).
    pub label: LocalizedText,
    /// The knob's current value, already fail-closed for the two string knobs.
    pub value: LocalizedText,
}

/// i18n label key for an `unknown_sender_mail` value (`family.value_*`).
fn unknown_sender_label_key(value: UnknownSenderMail) -> &'static str {
    match value {
        UnknownSenderMail::Allow => "family.value_allow",
        UnknownSenderMail::Hold => "family.value_hold",
        UnknownSenderMail::Reject => "family.value_reject",
    }
}

/// i18n label key for a `feed_sources` value (`family.value_*`). Deliberately a
/// **separate** map from [`unknown_sender_label_key`] — see [`FeedSources`] for
/// the merged-map trap this split retires.
fn feed_sources_label_key(value: FeedSources) -> &'static str {
    match value {
        FeedSources::Allow => "family.value_allow",
        FeedSources::Block => "family.value_block",
    }
}

/// i18n label key for an `unknown_peer_dm` value (`family.value_*`). A third
/// **separate** map, for the same reason [`feed_sources_label_key`] is one: this
/// knob's value set has no `reject`, and sharing [`unknown_sender_label_key`]
/// would let a `reject` stored under *this* knob render as "Reject" — a label
/// outside its own catalog.
fn unknown_peer_dm_label_key(value: UnknownPeerDm) -> &'static str {
    match value {
        UnknownPeerDm::Allow => "family.value_allow",
        UnknownPeerDm::Hold => "family.value_hold",
    }
}

/// The canonical `unknown_sender_mail` picker options (wire value + i18n label
/// key), in the ratified order. Derived from [`UnknownSenderMail::ORDER`] so the
/// value list cannot drift from what `fauna.family.policy.update` accepts.
pub fn unknown_sender_options() -> Vec<ReachPolicyOption> {
    UnknownSenderMail::ORDER
        .into_iter()
        .map(|v| ReachPolicyOption {
            value: v.as_str().to_string(),
            label: LocalizedText::key(unknown_sender_label_key(v)),
        })
        .collect()
}

/// Resolve a picker catalog's `label`s for display, in order — the
/// `into_iter().map(|o| o.label.resolve(lookup)).collect()` that tui's
/// `family.rs` and linux's `views/family.rs` each wrote independently for
/// [`unknown_sender_options`]/[`feed_sources_options`]/[`content_floor_options`]
/// (`&F` reborrows so `lookup` need not be `Copy`, matching
/// [`LocalizedText::resolve`]'s own generic-over-lookup design — every native
/// Rust client (a GUI toolkit's `ComboRow`/`StringList`, a terminal select) that
/// resolves through it, not just these three catalogs, can reuse this).
pub fn resolve_option_labels<F, S>(options: Vec<ReachPolicyOption>, lookup: F) -> Vec<String>
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    options
        .into_iter()
        .map(|o| o.label.resolve(&lookup))
        .collect()
}

/// [`unknown_sender_options`] resolved to display labels via `lookup`.
pub fn unknown_sender_option_labels<F, S>(lookup: F) -> Vec<String>
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    resolve_option_labels(unknown_sender_options(), lookup)
}

/// The canonical `feed_sources` picker options, in the ratified order.
pub fn feed_sources_options() -> Vec<ReachPolicyOption> {
    FeedSources::ORDER
        .into_iter()
        .map(|v| ReachPolicyOption {
            value: v.as_str().to_string(),
            label: LocalizedText::key(feed_sources_label_key(v)),
        })
        .collect()
}

/// [`feed_sources_options`] resolved to display labels via `lookup`.
pub fn feed_sources_option_labels<F, S>(lookup: F) -> Vec<String>
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    resolve_option_labels(feed_sources_options(), lookup)
}

/// The canonical `unknown_peer_dm` picker options, in the ratified order.
pub fn unknown_peer_dm_options() -> Vec<ReachPolicyOption> {
    UnknownPeerDm::ORDER
        .into_iter()
        .map(|v| ReachPolicyOption {
            value: v.as_str().to_string(),
            label: LocalizedText::key(unknown_peer_dm_label_key(v)),
        })
        .collect()
}

/// [`unknown_peer_dm_options`] resolved to display labels via `lookup`.
pub fn unknown_peer_dm_option_labels<F, S>(lookup: F) -> Vec<String>
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    resolve_option_labels(unknown_peer_dm_options(), lookup)
}

/// The localized label for a stored `unknown_sender_mail` wire value, looked up
/// via [`UnknownSenderMail::from_wire`] — so an unrecognized value renders as
/// **`hold`**, never the permissive `allow`.
///
/// This is the shared implementation of `family-safety.md` § Implementation
/// status' cross-app rule (*"an unrecognized value renders as the strictest
/// option (`hold` / `block`), never the permissive one … a **safety** rule, not a
/// cosmetic one"*), previously hand-rolled six times: linux's `FAIL_CLOSED_INDEX`,
/// web's `unknownSenderLabel`, apple's `FamilyVM` label map, android's
/// `normalizeUnknownSenderWire`, and windows' `NormalizeUnknownSenderWire`.
pub fn unknown_sender_label(value: &str) -> LocalizedText {
    LocalizedText::key(unknown_sender_label_key(UnknownSenderMail::from_wire(
        value,
    )))
}

/// The localized label for a stored `feed_sources` wire value, failing closed to
/// **`block`** — this knob's strict option. See [`unknown_sender_label`].
pub fn feed_sources_label(value: &str) -> LocalizedText {
    LocalizedText::key(feed_sources_label_key(FeedSources::from_wire(value)))
}

/// The localized label for a stored `unknown_peer_dm` wire value, failing closed
/// to **`hold`** — this knob's strict option. See [`unknown_sender_label`].
pub fn unknown_peer_dm_label(value: &str) -> LocalizedText {
    LocalizedText::key(unknown_peer_dm_label_key(UnknownPeerDm::from_wire(value)))
}

/// The five-line read-only reach-policy summary, in the ratified display order
/// (`family-safety.md` § Guardian policy pillar 1's knob table). All three string
/// knobs fail closed via [`unknown_sender_label`] / [`feed_sources_label`] /
/// [`unknown_peer_dm_label`].
///
/// Takes the knobs individually because `fauna-core` sits *below* `fauna-protocol`
/// and cannot name `ReachPolicy`; callers holding one should use
/// `ReachPolicy::summary_lines()`, which forwards here and keeps the bools from
/// being transposed at a call site.
///
/// `unknown_peer_dm` is `Option` because it rides the wire as an additive optional
/// field with absent-means-unchanged (the `content_notify` precedent, § Wire &
/// data shape → *Policy-update compatibility*). **Absent means the knob is at its
/// `allow` default** — an untouched policy (the nest omits the field at `allow`) —
/// which is not the unparseable case: an omitted knob genuinely gates no bridge
/// DMs, so rendering `allow` states what is actually enforced rather than failing
/// closed to a restriction nobody applied.
pub fn reach_policy_summary(
    contact_approval: bool,
    unknown_sender_mail: &str,
    federation_contact: bool,
    feed_sources: &str,
    unknown_peer_dm: Option<&str>,
) -> Vec<PolicySummaryLine> {
    fn on_off(b: bool) -> LocalizedText {
        LocalizedText::key(if b { "common.enable" } else { "common.disable" })
    }
    vec![
        PolicySummaryLine {
            label: LocalizedText::key("family.policy_contact_approval_label"),
            value: on_off(contact_approval),
        },
        PolicySummaryLine {
            label: LocalizedText::key("family.policy_unknown_sender_label"),
            value: unknown_sender_label(unknown_sender_mail),
        },
        PolicySummaryLine {
            label: LocalizedText::key("family.policy_federation_label"),
            value: on_off(federation_contact),
        },
        PolicySummaryLine {
            label: LocalizedText::key("family.policy_feed_sources_label"),
            value: feed_sources_label(feed_sources),
        },
        PolicySummaryLine {
            label: LocalizedText::key("family.policy_unknown_peer_dm_label"),
            value: unknown_peer_dm_label(unknown_peer_dm.unwrap_or(UnknownPeerDm::Allow.as_str())),
        },
    ]
}

/// i18n label key for a [`ContentFloor`] value (`family.value_*`). `Unknown`
/// (a value this client cannot parse) shares `block`'s key — the fail-closed
/// render rule (`family-safety.md` § Content policy). Its own separate map (not
/// reusing the reach-knob maps) because a content floor's value set is
/// `inherit / collapse / block`, disjoint from the reach knobs.
fn content_floor_label_key(value: ContentFloor) -> &'static str {
    match value {
        ContentFloor::Inherit => "family.value_inherit",
        ContentFloor::Collapse => "family.value_collapse",
        ContentFloor::Block | ContentFloor::Unknown => "family.value_block",
    }
}

/// i18n label key for a content-floor **category** name (`family.policy_content_*_label`),
/// used both by the guardian editor's four selects and the ward's summary. Any
/// name outside the four negative canonicals maps to a neutral flagged label.
fn content_category_label_key(category: &str) -> &'static str {
    match category {
        "nsfw" => "family.policy_content_nsfw_label",
        "spam" => "family.policy_content_spam_label",
        "phishing" => "family.policy_content_phishing_label",
        "commercial" => "family.policy_content_commercial_label",
        _ => "moderation.action.flagged",
    }
}

/// The canonical content-floor picker options (`inherit / collapse / block`, in
/// [`ContentFloor::ORDER`]), wire value + i18n label key. The guardian's four
/// per-category content selects (`family-policy-content-*-select`) render this one
/// catalog, so their value list cannot drift from what `fauna.family.policy.update`
/// accepts. Mirrors [`unknown_sender_options`] (`family-safety.md` § Content policy).
pub fn content_floor_options() -> Vec<ReachPolicyOption> {
    ContentFloor::ORDER
        .into_iter()
        .map(|v| ReachPolicyOption {
            value: v.as_str().to_string(),
            label: LocalizedText::key(content_floor_label_key(v)),
        })
        .collect()
}

/// [`content_floor_options`] resolved to display labels via `lookup`.
pub fn content_floor_option_labels<F, S>(lookup: F) -> Vec<String>
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    resolve_option_labels(content_floor_options(), lookup)
}

/// The localized label for a stored content-floor wire value, looked up via
/// [`ContentFloor::from_wire`] — so an unrecognized value renders **`block`**,
/// this knob's strict option, never the permissive `inherit`. See
/// [`unknown_sender_label`] for the shared safety rule.
pub fn content_floor_label(value: &str) -> LocalizedText {
    LocalizedText::key(content_floor_label_key(ContentFloor::from_wire(value)))
}

/// The ward-side read-only summary lines for a guardian **content policy**
/// (`family-safety.md` § Content policy — *"the ward's read-only policy summary
/// renders the content rules exactly as it renders the reach knobs"*). One line per
/// category whose floor is **not `inherit`**, in the canonical
/// [`GUARDIAN_FLOOR_CATEGORIES`] order — an `inherit` category adds nothing over the
/// ward's own preferences, so it is not a rule to display. Each value fails closed to
/// `block` via [`content_floor_label`]. Appended after the reach-knob summary by
/// `ReachPolicy::summary_lines()` (`fauna-protocol`).
pub fn content_policy_summary(policy: &ContentPolicy) -> Vec<PolicySummaryLine> {
    GUARDIAN_FLOOR_CATEGORIES
        .into_iter()
        .filter(|c| policy.floor_for(c) != ContentFloor::Inherit)
        .map(|c| PolicySummaryLine {
            label: LocalizedText::key(content_category_label_key(c)),
            value: content_floor_label(policy.floor_for(c).as_str()),
        })
        .collect()
}

/// The guardian's per-ward **Guardian Notify** readout line
/// (`family-ward-content-notices`, `family-safety.md` § Guardian Notify) for one
/// `(category, count)` notice on a `FamilyWardInfo`: `label` is the localized
/// category name (from the same [`content_category_label_key`] map the guardian
/// editor and the ward's summary use, so category names never drift), `value` is
/// the "N flagged today" count. **Category + count only — never content, never an
/// id** (the whole point of Notify). Reusing [`PolicySummaryLine`] means the
/// guardian readout renders through the same two-part label/value path the ward's
/// policy summary already uses (priority #2/#3), so no client adds a new shape.
pub fn content_notice_line(category: &str, count: u32) -> PolicySummaryLine {
    PolicySummaryLine {
        label: LocalizedText::key(content_category_label_key(category)),
        value: LocalizedText::key_arg(
            "family.ward_content_notice_count",
            "count",
            count.to_string(),
        ),
    }
}

/// The **screen-time usage readout** (`family-safety.md` § Screen time) for one
/// ward's day: `family-ward-usage-today` on the guardian's Family page, and the
/// very same line on the ward's own read-only summary.
///
/// One helper for both surfaces is the whole point. The goal doc requires that
/// *"the ward's summary shows the same number"* the guardian sees — a
/// transparency promise, and one that a second per-surface formatter would
/// eventually break by rounding, labelling or pluralizing differently. Reusing
/// [`PolicySummaryLine`] keeps it on the same two-part label/value path the
/// policy summary and [`content_notice_line`] already render through, so no app
/// adds a new shape (priority #2/#3).
///
/// `budget_minutes` is the guardian's `daily_minutes`: present, the value reads
/// "used of budget", which is what makes the number actionable to both of them;
/// absent, it degrades to the bare figure rather than inventing a denominator.
///
/// **Minutes are coarse metadata by design** — the same disclosure class as
/// `last_seen`. This line never names *what* was used, only how long.
pub fn usage_today_line(used_minutes: u32, budget_minutes: Option<u16>) -> PolicySummaryLine {
    let value = match budget_minutes {
        Some(budget) => LocalizedText::key_args(
            "family.ward_usage_today_of_budget",
            [
                ("used", used_minutes.to_string()),
                ("budget", budget.to_string()),
            ],
        ),
        None => LocalizedText::key_arg(
            "family.ward_usage_today",
            "minutes",
            used_minutes.to_string(),
        ),
    };
    PolicySummaryLine {
        label: LocalizedText::key("family.ward_usage_today_label"),
        value,
    }
}

/// What a `family-approval-item` row should display verbatim, or `None` when
/// the caller should render its own localized no-sender placeholder
/// (`family.approval_no_sender`).
///
/// Which field a kind reads, and why:
///
/// - **`mail_hold` / `dm_hold` → `peer_address`.** Envelope-class peer identity
///   the nest may see: a mail sender's address, or a bridge DM peer's external
///   id (`family-safety.md` § The mail gate / § The bridge-DM gate). Both kinds'
///   own `summary` is **always** empty — a subject line or message preview is
///   content, sealed to the ward — so binding it renders a blank row with live
///   Approve/Deny buttons; windows shipped exactly that defect once for
///   `mail_hold`, and `dm_hold` inherits the same shape.
/// - **`contact_request` → `peer_handle`.** The ward's ask is identified by
///   *who*, never *why*, so it deliberately carries no message text at all and
///   rides the nest-joined `peer_handle` instead (`family-safety.md`
///   § Child-initiated contact requests).
/// - **Everything else (`contact`, `feed_source`) → `summary`:** the knock's own
///   text, and the ward's own label for the source being approved.
///
/// **An empty field yields `None` for every kind**, not just the SMTP
/// null-reverse-path `mail_hold` that first motivated the rule
/// (`family-safety.md` § The mail gate, null-path rule): a blank row beside live
/// Approve/Deny buttons is the defect, and its causes are per-kind — a
/// null-path bounce, an off-nest `contact_request` peer with no local handle to
/// join, a knock sent without a summary. The caller renders its own localized
/// placeholder (`family.approval_no_sender`). Deciding is keyed on the entry's
/// own identifying fields (`message_id` for a mail hold, `peer_actor_id` for a
/// contact ask, `(bridge_id, peer_address)` for a DM hold), never on this text.
///
/// Takes the fields individually because `fauna-core` sits *below*
/// `fauna-protocol` and cannot name `FamilyApprovalEntry`; callers holding one
/// should use `FamilyApprovalEntry::display_text()`, which forwards here.
pub fn approval_display_text<'a>(
    kind: &str,
    peer_address: &'a str,
    peer_handle: &'a str,
    summary: &'a str,
) -> Option<&'a str> {
    let text = match kind {
        "mail_hold" | "dm_hold" => peer_address,
        "contact_request" => peer_handle,
        _ => summary,
    };
    if text.is_empty() { None } else { Some(text) }
}

/// One arm of `personalization-trained-factor-publish-kind-select`: the
/// artifact kind the publish writes, and the text the picker paints.
///
/// The [`backup_destination_kind_options`] shape verbatim — a raw-value picker
/// whose `value` is the wire discriminator and whose `label` is the same
/// [`LocalizedText`] every other surface uses for that kind. Sharing it is
/// priority #2 plus the specific select hazard that precedent records: **the
/// option a user picks and the words they read back must be the same text**,
/// and seven apps each hand-writing a two-item list is seven chances to drift.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PublishKindOption {
    /// The `artifact_kind` this option publishes as — never a per-app literal.
    pub value: String,
    /// The option text, deliberately the same [`LocalizedText`]
    /// [`publish_kind_label`] returns.
    pub label: LocalizedText,
}

/// The `personalization-trained-factor-publish-kind-select` option catalog, in
/// paint order (`docs/goal/behavior/topic-factors.md` § Publishing a trained
/// factor).
///
/// **List is first, and that is the default a shell lands on** — ui.yaml pins
/// it ("List | Model (default List)"), and the ordering is not cosmetic: a List
/// discloses only ids the publisher already made public, while a Model
/// discloses word patterns, so the weaker disclosure is what an unattended
/// return key picks.
///
/// [`crate::scoring::artifact_kind::WASM`] is **absent** rather than
/// present-and-disabled: a user cannot author a WASM module from this sheet at
/// all, and an unchoosable option teaches nothing (the deferred-S3-kind
/// reasoning in [`backup_destination_kind_options`]).
pub fn publish_kind_options() -> Vec<PublishKindOption> {
    [
        crate::scoring::artifact_kind::LIST,
        crate::scoring::artifact_kind::TEXT_MODEL,
    ]
    .into_iter()
    .map(|value| PublishKindOption {
        value: value.to_string(),
        label: publish_kind_label(value),
    })
    .collect()
}

/// The picker text for one publishable artifact kind.
///
/// An unrecognised kind interpolates the raw discriminator rather than
/// collapsing into a generic word, for [`backup_destination_kind_label`]'s
/// reason: a newer build's kind is exactly when the user needs to see *what*
/// this build cannot offer.
pub fn publish_kind_label(kind: &str) -> LocalizedText {
    match kind {
        crate::scoring::artifact_kind::LIST => {
            LocalizedText::key("personalization.publish_kind_list")
        }
        crate::scoring::artifact_kind::TEXT_MODEL => {
            LocalizedText::key("personalization.publish_kind_model")
        }
        other => LocalizedText::key_arg(
            "personalization.publish_kind_unknown",
            "kind",
            other.to_string(),
        ),
    }
}

/// The `labeler-catalog-item-kind` badge's **override** text, or `None` when
/// the row should paint its raw `artifact_kind` discriminator unchanged.
///
/// This is the "and says so" half of the unknown-version contract
/// (`content-moderation-and-ranking.md` § Tier-3 artifact kinds): a subscribed
/// `text-model` whose tokenizer contract this build does not implement is left
/// **inert** by the compose seam, and the catalog row is where the user is told
/// why — the feed itself is correct, just missing a factor, so a feed banner
/// would be a lie about the feed.
///
/// **`None` means "paint the kind verbatim", not "no badge".** ui.yaml pins this
/// element's text to the raw discriminator (`list` | `wasm` | `text-model`) so a
/// driver reads a stable value rather than a translated one; only the
/// unsupported case substitutes a sentence, and only that sentence needs to be
/// one wording across seven apps — which is the whole reason it lives here
/// rather than in each shell.
///
/// `artifact_version == 0` is **absent, not unsupported**: every
/// non-text-model artifact makes no version claim at all, so the row keeps
/// today's behaviour (`LabelerSummary::artifact_version` documents the wire
/// side). Accusing an artifact of being too new because it carries no version
/// would be exactly backwards.
pub fn text_model_needs_newer_app(
    artifact_kind: &str,
    artifact_version: u64,
) -> Option<LocalizedText> {
    if artifact_kind != crate::scoring::artifact_kind::TEXT_MODEL || artifact_version == 0 {
        return None;
    }
    let claimed = u16::try_from(artifact_version).unwrap_or(u16::MAX);
    if crate::scoring::text_model_version_supported(claimed) {
        return None;
    }
    Some(LocalizedText::key("labeler_catalog.kind_needs_newer_app"))
}

/// The Model review row's class-direction text
/// (`personalization-trained-factor-publish-ngram-direction`, and the
/// `labeler-inspect-model-entry-direction` twin at the subscriber's end).
///
/// § Publishing ratifies the direction column as part of the disclosure — "the
/// dislike half being part of the disclosure the direction column makes
/// visible" — so a row whose n-gram is dominated by *less like this* examples
/// must say so in words, not leave the reader to compare two numbers. A tie
/// reads as *both*: it genuinely occurs in equal numbers of each class, and
/// rounding it to either side would misstate what the publisher is disclosing.
///
/// Shared because both surfaces render the same artifact counts and the two
/// must not disagree about what "more" means — the publisher's review is a
/// promise about what a subscriber reads back at inspect.
pub fn ngram_direction_label(more: u32, less: u32) -> LocalizedText {
    match more.cmp(&less) {
        core::cmp::Ordering::Greater => {
            LocalizedText::key("personalization.publish_ngram_direction_more")
        }
        core::cmp::Ordering::Less => {
            LocalizedText::key("personalization.publish_ngram_direction_less")
        }
        core::cmp::Ordering::Equal => {
            LocalizedText::key("personalization.publish_ngram_direction_both")
        }
    }
}

/// The Model review row's distinct-document count
/// (`personalization-trained-factor-publish-ngram-count`, and the
/// `labeler-inspect-model-entry-count` twin).
///
/// The number is `more + less` — the **class-blind** distinct-document count,
/// which is the quantity [`crate::scoring::TEXT_MODEL_PUBLISH_MIN_DOCS`] bounds
/// (`topic-factors.md` § Publishing: "counting both classes together — the
/// prune is an anti-quote privacy floor"). Rendering only the dominant class's
/// count would understate the disclosure and could print a number *below* the
/// floor for a row that legitimately survived it.
pub fn ngram_doc_count_label(more: u32, less: u32) -> LocalizedText {
    LocalizedText::key_arg(
        "personalization.publish_ngram_count",
        "count",
        more.saturating_add(less).to_string(),
    )
}

#[cfg(test)]
mod publish_kind_tests {
    use super::*;

    fn arg<'a>(t: &'a LocalizedText, key: &str) -> Option<&'a str> {
        t.args
            .iter()
            .find(|(k, _)| k.as_str() == key)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn the_catalog_offers_exactly_the_two_publishable_kinds_list_first() {
        assert_eq!(
            publish_kind_options()
                .iter()
                .map(|o| o.value.clone())
                .collect::<Vec<_>>(),
            vec!["list".to_string(), "text-model".to_string()],
            "List first — the weaker disclosure is what an unattended default picks"
        );
    }

    #[test]
    fn every_option_label_is_the_same_text_the_label_fn_returns() {
        // The drift this catalog exists to prevent: pick "Model", read back
        // some other word on the row it produced.
        for o in publish_kind_options() {
            assert_eq!(o.label, publish_kind_label(&o.value), "option {}", o.value);
        }
    }

    #[test]
    fn a_text_model_this_build_can_score_keeps_its_raw_kind_badge() {
        // `None` is load-bearing: ui.yaml pins the badge to the raw
        // discriminator, and both e2e journeys assert `== "text-model"` on it.
        assert_eq!(
            text_model_needs_newer_app(
                crate::scoring::artifact_kind::TEXT_MODEL,
                crate::scoring::TEXT_MODEL_ARTIFACT_VERSION as u64,
            ),
            None
        );
    }

    #[test]
    fn a_text_model_from_the_future_says_it_needs_a_newer_app() {
        let t = text_model_needs_newer_app(
            crate::scoring::artifact_kind::TEXT_MODEL,
            crate::scoring::TEXT_MODEL_ARTIFACT_VERSION as u64 + 1,
        )
        .expect("an unimplemented tokenizer contract must surface on the badge");
        assert_eq!(t.key, "labeler_catalog.kind_needs_newer_app");
    }

    #[test]
    fn version_zero_is_an_absent_claim_not_an_unsupported_one() {
        // A non-text-model artifact states no version at all. Reading that as
        // "too new" would light the badge on every such row, which is the
        // regression this case exists to prevent.
        assert_eq!(
            text_model_needs_newer_app(crate::scoring::artifact_kind::TEXT_MODEL, 0),
            None
        );
    }

    #[test]
    fn only_the_text_model_kind_can_ever_need_a_newer_app() {
        // A `list` or `wasm` row carries no tokenizer contract, so a non-zero
        // version on one is meaningless rather than alarming.
        for kind in [
            crate::scoring::artifact_kind::LIST,
            crate::scoring::artifact_kind::WASM,
        ] {
            assert_eq!(text_model_needs_newer_app(kind, 9_999), None, "kind {kind}");
        }
    }

    #[test]
    fn the_badge_and_the_scorer_read_one_predicate() {
        // The disagreement this slice must make unexpressible: a badge that
        // said "needs a newer app" for an artifact the seam happily scores, or
        // stayed silent for one it leaves inert. Walk both sides over the same
        // versions and assert they never disagree.
        for v in 0u16..8 {
            let scorable = crate::scoring::text_model_version_supported(v);
            let badged =
                text_model_needs_newer_app(crate::scoring::artifact_kind::TEXT_MODEL, v as u64)
                    .is_some();
            // v == 0 is the one asymmetry, and it is deliberate: not scorable
            // (no such contract), not badged (no claim was made).
            if v == 0 {
                assert!(!scorable && !badged, "v0 is absent on both sides");
            } else {
                assert_eq!(scorable, !badged, "version {v} disagrees across the two");
            }
        }
    }

    #[test]
    fn an_unknown_kind_shows_the_raw_discriminator() {
        let t = publish_kind_label("quantum-vibes");
        assert_eq!(t.key, "personalization.publish_kind_unknown");
        assert_eq!(arg(&t, "kind"), Some("quantum-vibes"));
    }

    #[test]
    fn direction_names_the_dominant_class_and_a_tie_is_both() {
        assert_eq!(
            ngram_direction_label(3, 1).key,
            "personalization.publish_ngram_direction_more"
        );
        assert_eq!(
            ngram_direction_label(1, 3).key,
            "personalization.publish_ngram_direction_less"
        );
        // A tie is not roundable: 2 and 2 is a genuine both-classes pattern,
        // and the dislike half is part of the ratified disclosure.
        assert_eq!(
            ngram_direction_label(2, 2).key,
            "personalization.publish_ngram_direction_both"
        );
    }

    #[test]
    fn the_doc_count_is_class_blind_never_the_dominant_class_alone() {
        // 2 + 1 = 3 survives TEXT_MODEL_PUBLISH_MIN_DOCS; printing the dominant
        // class alone would show "2" for a row the floor legitimately admitted.
        assert_eq!(arg(&ngram_doc_count_label(2, 1), "count"), Some("3"));
    }
}

#[cfg(test)]
mod approval_display_text_tests {
    use super::*;

    #[test]
    fn mail_hold_renders_peer_address_not_the_empty_summary() {
        assert_eq!(
            approval_display_text("mail_hold", "stranger@example.com", "", ""),
            Some("stranger@example.com"),
        );
    }

    #[test]
    fn contact_renders_its_summary() {
        assert_eq!(
            approval_display_text("contact", "", "", "hi from bob"),
            Some("hi from bob"),
        );
    }

    #[test]
    fn a_null_path_mail_hold_yields_none() {
        assert_eq!(approval_display_text("mail_hold", "", "", "anything"), None);
    }

    /// `family-safety.md` § Child-initiated contact requests — the entry
    /// *deliberately* carries no `summary` ("identified by *who*, never *why*")
    /// and rides the nest-joined `peer_handle` instead, so binding `summary`
    /// renders a blank row beside live Approve/Deny buttons: the exact defect
    /// windows once shipped for `mail_hold`.
    #[test]
    fn contact_request_renders_its_peer_handle_not_the_empty_summary() {
        assert_eq!(
            approval_display_text("contact_request", "", "alice", ""),
            Some("alice"),
        );
    }

    /// `family-safety.md` § The bridge-DM gate — a held conversation's entry
    /// carries the external peer id on `peer_address` and an *always* empty
    /// `summary` (a preview would be content the guardian must never see), so
    /// this kind reads `peer_address` exactly as `mail_hold` does.
    #[test]
    fn dm_hold_renders_its_peer_address_not_the_empty_summary() {
        assert_eq!(
            approval_display_text("dm_hold", "npub1stranger", "", ""),
            Some("npub1stranger"),
        );
    }

    /// The invariant the two new kinds generalize: whatever field a kind reads,
    /// an **empty** one never renders as a blank row — the caller gets `None`
    /// and shows its localized placeholder. Previously only `mail_hold` was
    /// guarded, so an off-nest `contact_request` peer (no local handle to join)
    /// or a summary-less knock still reached the row blank.
    #[test]
    fn every_kind_yields_none_rather_than_an_empty_row() {
        for kind in [
            "mail_hold",
            "dm_hold",
            "contact_request",
            "contact",
            "feed_source",
            "a_kind_this_client_does_not_know",
        ] {
            assert_eq!(
                approval_display_text(kind, "", "", ""),
                None,
                "{kind} must not render an empty row",
            );
        }
    }
}

/// True when a contact roster row (`fauna.contacts.list`) represents the
/// `target_actor_id` being **blocked**: the row's `peer_id` identifies the target
/// AND its `status` is `blocked`. Clients fold this over their roster with
/// `.any(..)` / `.some(..)` to derive the `profile-block-button` toggle state, which
/// [`contact_toggle_block_label`] then labels.
///
/// The actor-id match is **case-insensitive**: actor IDs are canonical lowercase hex,
/// so an ASCII-case-insensitive compare never narrows a real match and tolerates a
/// non-normalized id — this unifies windows' prior case-insensitive compare with the
/// other four apps' case-sensitive one (the lone behavioral divergence this lift
/// resolves, priority #4). Mirrors the per-row scalar shape of [`contact_matches_filter`]
/// so the client keeps its idiomatic iteration; see contacts.md § Where logic lives
/// → Unblock and § Persistence (the roster row carries `status`).
pub fn contact_row_blocks_actor(
    row_peer_id: &str,
    row_status: &str,
    target_actor_id: &str,
) -> bool {
    row_status == "blocked" && row_peer_id.eq_ignore_ascii_case(target_actor_id)
}

/// The Devices page `device-card`'s `device-status` label: `devices.online`
/// (`"Online"`) when the device's `online` flag is set, else `devices.offline`
/// (`"Offline"`), as a [`LocalizedText`] each app resolves through its own i18n
/// pipeline. A pure function of the `fauna.sync.devices.list` row's `online` bool —
/// no wire change.
///
/// Shared so the online → key contract can't drift per-app. Previously hand-rolled
/// divergently: **windows** hardcoded the untranslated literals `"Online"`/`"Offline"`
/// (priority #1 violation), while web/linux/apple/android already resolved the canonical
/// `devices.{online,offline}` keys (en.yaml). This converges all six on the shared map,
/// resolving the windows violation — the same bool→2-key shape as
/// [`contact_toggle_block_label`], and the next member of the device-presentation family
/// already lifted (`fauna_folders_machine::frequency_label`). The
/// status **dot color** (linux's `success`/`dim-label`, apple's green/gray) stays an
/// idiomatic per-app render — the same split as the status icon/color in
/// [`contact_status_label`]. See devices.md § Where logic lives and § Element table
/// (`device-status`).
pub fn device_status_label(online: bool) -> LocalizedText {
    if online {
        LocalizedText::key("devices.online")
    } else {
        LocalizedText::key("devices.offline")
    }
}

/// The devices-page `device-folder-role-badge` chip text for one device place
/// (`DeviceFolderRole`'s three flags) — **composed** from the same three
/// `devices.wizard.place_*` labels the create wizard's place checkboxes carry,
/// never a name for a point, so every one of the eight points gets honest text
/// and no role vocabulary returns (`folders.md` § Implementation status today,
/// the role contraction). One flag is its own label; two or three are joined
/// by the `devices.place_two` / `devices.place_three` templates, whose
/// arguments are themselves keys — render with a nested resolve
/// ([`LocalizedText::resolve_nested`] and each app's twin); a place with no
/// flag set reads `devices.place_none`. The ui.yaml element keeps its
/// already-approved `device-folder-role-badge` spelling.
pub fn device_place_label(originates: bool, accepts: bool, applies_deletes: bool) -> LocalizedText {
    let parts: Vec<&str> = [
        (originates, "devices.wizard.place_originates"),
        (accepts, "devices.wizard.place_accepts"),
        (applies_deletes, "devices.wizard.place_applies_deletes"),
    ]
    .into_iter()
    .filter_map(|(set, key)| set.then_some(key))
    .collect();
    match parts.as_slice() {
        [] => LocalizedText::key("devices.place_none"),
        [one] => LocalizedText::key(*one),
        [first, second] => LocalizedText::key_args(
            "devices.place_two",
            [("first", *first), ("second", *second)],
        ),
        [first, second, third, ..] => LocalizedText::key_args(
            "devices.place_three",
            [("first", *first), ("second", *second), ("third", *third)],
        ),
    }
}

/// A Nostr Connect (NIP-46 bunker) roster row's primary label
/// (`nostr-bunker-app-item`'s name text): `label` verbatim if the app has set
/// one, else a status-derived placeholder — `nostr.connected_apps.pending`
/// while the connection hasn't completed, else `nostr.connected_apps.unnamed`.
/// The bunker flow transports no app name at connect time, so a fresh row is
/// always unlabeled until the app itself sets one later. `label`/`status` are
/// the `BunkerAppEntry` wire fields verbatim (`fauna_protocol::nostr`).
///
/// Shared so the label ↔ status contract can't drift per-app — previously
/// hand-rolled identically in web/linux/tui (each cross-referencing the others
/// in a doc comment to stay in sync by hand, the tell that this belongs here
/// instead). android hand-rolled its own render-side copy instead of
/// consuming this face; apple consumes it via `libs/fauna-ffi/src/
/// nostr_client.rs::bunker_app_label`. windows has no Connected Apps UI yet.
/// The non-empty-label arm reuses the verbatim-passthrough trick
/// [`contact_status_label`]'s unknown-status arm uses: an arbitrary label
/// string is very unlikely to collide with a real i18n key, so `resolve`'s
/// key-as-fallback behavior renders it unchanged.
pub fn bunker_app_label(label: &str, status: &str) -> LocalizedText {
    if !label.is_empty() {
        LocalizedText::key(label)
    } else if status == "pending" {
        LocalizedText::key("nostr.connected_apps.pending")
    } else {
        LocalizedText::key("nostr.connected_apps.unnamed")
    }
}

/// A Nostr Connect (NIP-46 bunker) roster row's last-used sub-label: absent
/// (`None`) → `nostr.connected_apps.never_used`; present → the caller's
/// already-formatted time string as the `nostr.connected_apps.last_used`
/// `{time}` argument. The formatted-time string is a caller responsibility,
/// same split as every other date/time field in this module — each platform
/// resolves "local" through its own OS API (see the epoch/local-time note on
/// [`format_unix_local`] for why that can't move into pure Rust); only the
/// *which-key* decision is shared.
///
/// Shared so the presence → key contract can't drift per-app — the same
/// `Option`-driven 2-key shape as [`device_status_label`]'s bool→2-key one;
/// previously hand-rolled identically in web/linux/tui.
pub fn bunker_last_used_label(formatted_time: Option<&str>) -> LocalizedText {
    match formatted_time {
        Some(time) => LocalizedText::key_arg("nostr.connected_apps.last_used", "time", time),
        None => LocalizedText::key("nostr.connected_apps.never_used"),
    }
}

/// The admin Users page `admin-users-mail-serving-status[i]` read-only audit
/// indicator: `admin.users_page.serving_here` (`"Serving here"`) when the user's
/// local IMAP/CalDAV serving is enabled, else `admin.users_page.serving_disabled`
/// (`"Not serving"`), as a [`LocalizedText`] each app resolves through its own
/// i18n pipeline. A pure function of the `AdminClient::users_list` reply row's
/// `AdminUser.mail_serving_enabled` bool — view-only: the *user* sets it from their
/// own mail-settings serve-here toggle, the admin never does (admin.md § Users →
/// `admin-users-mail-serving-status`; no wire change).
///
/// Shared so the enabled → key contract can't drift per-app — the same
/// bool→2-key shape as [`device_status_label`] / [`contact_toggle_block_label`],
/// the label family already lifted. Previously hand-rolled identically in all five
/// apps (windows/linux/web/apple/android each computing `enabled ? serving_here
/// : serving_disabled` against its own string table); this single-sources the
/// decision. A client keeps only its own null-default glue (web's
/// `mail_serving_enabled ?? true`, since the wire field defaults on) and passes the
/// resulting bool in. See value-formatting.md § Implementation status today.
/// The §5 manual-claims list `subscription-claim-status[i]` badge: a claim
/// code reads `subscriptions.claim_status_redeemed` ("Redeemed") once some
/// actor has bound it, `subscriptions.claim_status_voided` ("Voided") if it
/// was voided before anyone redeemed it, else
/// `subscriptions.claim_status_unredeemed` ("Unredeemed").
///
/// The sibling of [`device_status_label`] / [`mail_serving_status_label`],
/// with one extra state. **Redeemed wins over voided** — a code redeemed and
/// later voided reads "Redeemed", because the redemption is the fact the
/// author is auditing for. That precedence is the reason this is shared
/// rather than hand-rolled per client: the two booleans are independent on
/// the wire (`ClaimItem.redeemed_by` / `.voided_at`), so a client branching
/// in the other order would disagree with its siblings on the same row and
/// nothing would catch it.
///
/// See monetization.md § Pillar 3 (`claims.list` — the audit surface for both
/// manually- and webhook-minted codes).
pub fn claim_status_label(redeemed: bool, voided: bool) -> LocalizedText {
    if redeemed {
        LocalizedText::key("subscriptions.claim_status_redeemed")
    } else if voided {
        LocalizedText::key("subscriptions.claim_status_voided")
    } else {
        LocalizedText::key("subscriptions.claim_status_unredeemed")
    }
}

/// The §4 payment-provider row status badge (monetization.md § Pillar 3 →
/// "Provider status — evidence-based, no ping", ratified 2026-07-16):
/// evidence-based, never an active probe. Both `None` (no webhook delivery
/// seen yet) → `configured`; `last_rejected_at >= last_verified_at`
/// (most-recent evidence wins; a tie resolves conservative, toward `error`)
/// → `error`; else `verified`. One shared decision so no client re-derives
/// the branch (the `claim_status_label` precedent).
pub fn provider_status_label(
    last_verified_at: Option<u64>,
    last_rejected_at: Option<u64>,
) -> LocalizedText {
    match last_rejected_at {
        // No rejection ever recorded → verified iff a verification was, else
        // no evidence at all yet.
        None => match last_verified_at {
            Some(_) => LocalizedText::key("subscriptions.provider_status_verified"),
            None => LocalizedText::key("subscriptions.provider_status_configured"),
        },
        // A rejection exists: it wins unless a STRICTLY later verification
        // superseded it (a tie, or no verification at all, resolves to error).
        Some(rejected) => match last_verified_at {
            Some(verified) if verified > rejected => {
                LocalizedText::key("subscriptions.provider_status_verified")
            }
            _ => LocalizedText::key("subscriptions.provider_status_error"),
        },
    }
}

pub fn mail_serving_status_label(enabled: bool) -> LocalizedText {
    if enabled {
        LocalizedText::key("admin.users_page.serving_here")
    } else {
        LocalizedText::key("admin.users_page.serving_disabled")
    }
}

/// The `admin-dns` record-matrix per-record verdict label: the live
/// `fauna.dns.verify_records` verdict (`fauna_client_dns::VerifyStatus`, whose
/// serde variant name crosses as `status`) maps to `admin.dns.status_*`, as a
/// [`LocalizedText`] each app resolves through its own i18n pipeline. `Ok` →
/// `status_ok`, `Missing` → `status_missing`, `Mismatch` → `status_mismatch`;
/// **anything else** — `Checking`, an absent verdict (`None`, not yet verified),
/// or a wire/version-drift value — reads as the neutral `status_checking`, never
/// a false green/red (the same "missing verdict = checking" rule linux's
/// `verdict_text_and_class` and web's `statusLabel` already carry).
///
/// Takes the serde variant name as `&str` (not the typed enum) because
/// `fauna_core` does not depend on `fauna-protocol`/`fauna-client-dns`. Shared
/// so the verdict → key decision can't drift per-app; previously hand-rolled
/// identically (linux
/// `verdict_text_and_class`, web `statusLabel`, windows `AdminDnsPage`). The
/// verdict **CSS class** (`ok`/`bad`/`checking` — the red/green colour) stays an
/// idiomatic per-app render, the same split as the status icon/color in
/// [`contact_status_label`]. See value-formatting.md § DNS verdict label.
///
/// `observed` is the record's `RecordVerdict.observed` — what public DNS actually
/// served at that name, which the nest already resolves and puts on the wire. A
/// `Mismatch` renders it (`status_mismatch_found`, "Mismatch — found 1.2.3.4"),
/// because a red verdict that cannot say *mismatch with what* is a dead-end
/// diagnostic. The other verdicts do not: `Missing` is `observed.is_empty()` by
/// construction (`fauna_mail::dns::verify`), and a green row needs no forensics.
/// A `Mismatch` that arrives with no observed values — only reachable from a nest
/// too old to send them — degrades to the plain `status_mismatch`, so the label
/// stays correct across a version skew.
pub fn dns_verdict_label(status: &str, observed: &[String]) -> LocalizedText {
    match status {
        "Ok" => LocalizedText::key("admin.dns.status_ok"),
        "Missing" => LocalizedText::key("admin.dns.status_missing"),
        "Mismatch" if !observed.is_empty() => LocalizedText::key_arg(
            "admin.dns.status_mismatch_found",
            "found",
            observed.join(", "),
        ),
        "Mismatch" => LocalizedText::key("admin.dns.status_mismatch"),
        _ => LocalizedText::key("admin.dns.status_checking"),
    }
}

/// The `admin-dns` served-cert health badge's **state** sub-label: the nest's
/// `fauna.tls.cert_status` `CertHealthState` (whose serde variant name crosses as
/// `state`) maps to `admin.dns.cert.status_*`, as a [`LocalizedText`] each app
/// resolves through its own i18n pipeline. `ValidTrusted` → `status_valid`,
/// `Expiring` → `status_expiring`; **anything else** — `OnFloorRenewNeeded` (the
/// fresh-nest default) or a wire/version-drift value — reads as `status_on_floor`
/// ("Renew needed"), matching web's `certStatusText` default arm and linux's
/// `cert_status_text`. Only the state word is mapped here; [`cert_status_view`]
/// decides the sub-label that follows it and is what a badge should call —
/// this function stays the state → key primitive it composes.
///
/// Takes the serde variant name as `&str` (not the typed enum) because
/// `fauna_core` does not depend on `fauna-protocol`. Shared so the state → key
/// decision can't drift per-app; previously hand-rolled identically (linux
/// `cert_status_text`, web `certStatusText`, windows `AdminDnsPage`). The
/// badge **CSS class**
/// (`ok`/`warn`/`checking`) stays an idiomatic per-app render, the same split
/// as [`contact_status_label`]. See value-formatting.md § DNS verdict label and
/// tls-certificates.md § C.4.
pub fn cert_status_label(state: &str) -> LocalizedText {
    match state {
        "ValidTrusted" => LocalizedText::key("admin.dns.cert.status_valid"),
        "Expiring" => LocalizedText::key("admin.dns.cert.status_expiring"),
        _ => LocalizedText::key("admin.dns.cert.status_on_floor"),
    }
}

/// Consecutive failed attempts to *establish* a nest connection before a client
/// stops calling the gap transient and reports the `Unreachable` connection
/// state. **The single owner of this threshold** — the native supervisor
/// (`fauna_ws_substrate::supervisor`) and the wasm reconnect loop
/// (`fauna_rpc_wasm`) are two implementations of one contract, and a per-loop
/// copy is exactly the cross-implementation drift this codebase has been bitten
/// by. Rationale for the value + the wall-clock it works out to lives with the
/// state itself (transport.md § Connection-status indicator).
pub const CONNECTION_UNREACHABLE_AFTER_CONSECUTIVE_FAILURES: u32 = 8;

/// The global `connection-status` indicator's label: the transport
/// `ConnectionState` — as the lowercase wire word every app family already
/// carries (`"connected"`, `"connecting"`, `"disconnected"`, `"unreachable"`) —
/// mapped to a `common.*` [`LocalizedText`] each app resolves through its own
/// i18n pipeline.
///
/// `unreachable` is the fourth state (added 2026-07-29): connecting has failed
/// [`CONNECTION_UNREACHABLE_AFTER_CONSECUTIVE_FAILURES`] times in a row with no
/// established connection in between, so the gap is not a Watchtower swap and
/// the indicator says "Cannot connect" rather than an indefinite "Connecting…".
/// **Anything unrecognised reads as `disconnected`** — an older client meeting a
/// newer state word degrades to the honest weaker claim rather than a blank
/// indicator (version-compatibility.md § additive-everywhere).
///
/// Takes the state as `&str` (not the typed enum) for the same reason
/// [`cert_status_label`] does: `fauna_core` depends on neither the native
/// transport crate nor `fauna-protocol`. Shared so the state → key decision
/// cannot drift per-app — it was hand-rolled seven times before this (linux
/// `client.rs`, tui `ui.rs`, web `+layout.svelte`, android `FaunaNavHost.kt`,
/// windows `NestRpcClient.cs`, and FaunaKit for macOS + iOS).
pub fn connection_state_label(state: &str) -> LocalizedText {
    match state {
        "connected" => LocalizedText::key("common.connected"),
        "connecting" => LocalizedText::key("common.connecting"),
        "unreachable" => LocalizedText::key("common.cannot_connect"),
        _ => LocalizedText::key("common.disconnected"),
    }
}

/// The whole `admin-dns` served-cert badge, decided once: the state word plus
/// **which** of the two mutually-exclusive sub-labels follows it. Clients render
/// `"{admin.dns.cert.label} {state}"`, then append `({admin.dns.cert.self_signed})`
/// when `show_self_signed`, or `admin.dns.cert.expires{date}` when
/// `expires_at_unix` is `Some` — formatting that epoch with the platform's native,
/// locale-aware date formatter, the same split as [`RelativeTimeDisplay`].
///
/// The two are never both set: a self-signed floor cert's own expiry is not the
/// admin's concern — what it needs is a *trusted* cert — so the date is withheld
/// and the badge says `self-signed` instead. This is the decision the per-app
/// copies drifted on, and the reason it is no longer theirs to make: the floor
/// cert carries a genuine, far-future `not_after` (rcgen's default 4096-01-01;
/// `bins/fauna-nest/src/acme.rs` mints it with no override), so testing `is_floor`
/// and `not_after_unix > 0` *independently* — as apple's `AdminDnsView` did —
/// renders "Certificate: Renew needed (self-signed) — expires 4096-01-01" on
/// every TLS-serving fresh nest: a two-millennium expiry reading as reassurance
/// on an untrusted cert.
///
/// value-formatting.md § Cert status badge; tls-certificates.md § C.4 (the row's
/// three states) and § Where logic lives ("per-app shells … no cert logic of
/// their own"). The badge's colour stays an idiomatic per-app render, the same
/// split as [`cert_status_label`] and [`contact_status_label`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CertStatusView {
    /// The state word — [`cert_status_label`]'s key, so one call resolves the
    /// whole badge and no client re-derives the state → key map.
    pub state: LocalizedText,
    /// Append the `(self-signed)` sub-label — the cert is the floor.
    pub show_self_signed: bool,
    /// `Some(epoch)` iff the expiry date belongs on the badge: a trusted cert
    /// (valid or expiring) whose `not_after_unix` the nest actually reported.
    /// `None` for the floor (see above) and for the `0` a nest with no TLS
    /// resolver at all sends.
    pub expires_at_unix: Option<i64>,
}

/// [`CertStatusView`] from the three fields `fauna.tls.cert_status` reports per
/// domain (`DomainCertStatus` → the client-side `CertStatusRow`).
///
/// Takes the serde variant name as `&str` for the same reason [`cert_status_label`]
/// does — `fauna_core` does not depend on `fauna-protocol`.
pub fn cert_status_view(state: &str, is_floor: bool, not_after_unix: i64) -> CertStatusView {
    CertStatusView {
        state: cert_status_label(state),
        show_self_signed: is_floor,
        // Mutually exclusive by construction — the invariant every app relies
        // on, rather than five hand-written `else if` chains that must each stay
        // in that shape forever.
        expires_at_unix: (!is_floor && not_after_unix > 0).then_some(not_after_unix),
    }
}

/// The viewer's subscription status for one offered tier — the
/// `subscription-offers-section` per-tier status badge on **another** actor's
/// profile (`docs/goal/ui/profile.md` § Layout & flow → *Another's profile*;
/// `docs/goal/behavior/monetization.md` § Pillar 1). A *derived* viewmodel enum
/// (not a wire type): the offers browse reads `offers.list` (`Vec<TierItem>`) +
/// `status.get` (`StatusGetReply { tier: Option<String>, .. }`), and per tier
/// derives this status. `status.get` carries **no** pending discriminant — an
/// OTHER profile sees only the viewer's *confirmed* tier — so `Pending` is purely
/// a transient post-click state (subscribe → `Queued`), tracked client-side.
///
/// Lifts the five per-app derivations onto one source of truth (priority
/// #2/#4): web's inline map, linux's split 2-case `status_text` + a separate
/// post-click overlay, and the android/apple/windows **local** `OfferStatus` /
/// `OfferStatusKind` enums — all a strict superset, converged here on the richest
/// (native-enum) shape. Clients branch on this enum for the label **and** (where
/// they choose to) the Subscribe button state; the button enable/disable *policy*
/// stays an idiomatic per-app UX choice (android disables on `Active`; web /
/// linux / apple / windows leave it enabled). See profile.md / monetization.md
/// § Where logic lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum OfferStatus {
    /// The viewer neither holds nor has a pending request for this tier.
    None,
    /// A transient post-click `Queued` state (encrypted-mode subscribe), shown
    /// until the next `status.get` read confirms or drops it.
    Pending,
    /// The viewer's confirmed held tier (`status.get` reports this tier).
    Active,
}

/// Derive the per-tier [`OfferStatus`]. Precedence **Active > Pending > None**:
/// the viewer's confirmed `status_tier` (from `status.get`) wins, else a transient
/// post-click `pending` flag, else none. Centralizes the precedence the five
/// apps each hand-rolled. See [`OfferStatus`].
pub fn offer_status(tier_name: &str, status_tier: Option<&str>, pending: bool) -> OfferStatus {
    if status_tier == Some(tier_name) {
        OfferStatus::Active
    } else if pending {
        OfferStatus::Pending
    } else {
        OfferStatus::None
    }
}

/// Map an [`OfferStatus`] to its badge label key (`subscriptions.offer_status_*`),
/// as a [`LocalizedText`] each app resolves through its own i18n pipeline. The
/// status icon/color (where a client renders one) stays an idiomatic per-app
/// render — the same split as [`contact_status_label`].
pub fn offer_status_label(status: OfferStatus) -> LocalizedText {
    match status {
        OfferStatus::None => LocalizedText::key("subscriptions.offer_status_none"),
        OfferStatus::Pending => LocalizedText::key("subscriptions.offer_status_pending"),
        OfferStatus::Active => LocalizedText::key("subscriptions.offer_status_active"),
    }
}

/// The shared **six-state** per-file sync-status display vocabulary — what the
/// `sync-state-badge` on the media page tells the user about where a file lives
/// (file-sync.md § Per-file sync-status display). Each desktop-engine client
/// (apple `SyncEngine`+SwiftData, android Room, the Windows cfapi host) derives
/// the badge from its own local engine and converges the **label text** here;
/// the badge icon/color stays an idiomatic per-app render (the same
/// render-vs-label split as [`offer_status_label`] / [`contact_status_label`]).
/// Control-plane clients (web, the Linux media page) have no local engine and
/// render only `Synced`. The engine-internal eight-variant
/// `fauna_sync_engine::SyncState` collapses onto these six via
/// `SyncState::to_display()` (engine-holding clients only — today Windows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum SyncDisplayState {
    /// Present and up to date both locally and on the nest.
    Synced,
    /// Present on this device, not yet uploaded.
    LocalOnly,
    /// On the nest only — a placeholder on this device (on-demand), not hydrated.
    RemoteOnly,
    /// Local changes are being pushed.
    Uploading,
    /// Remote bytes are being pulled / hydrated.
    Downloading,
    /// Local and remote diverged; resolve on the Peers/Devices page.
    Conflict,
}

/// Map a [`SyncDisplayState`] to its badge label key (`media.status_label.*`), as a
/// [`LocalizedText`] each app resolves through its own i18n pipeline. The badge
/// icon/color (where a client renders one) stays an idiomatic per-app render —
/// the same split as [`offer_status_label`]. See file-sync.md § Per-file sync-status
/// display.
pub fn sync_display_state_label(state: SyncDisplayState) -> LocalizedText {
    match state {
        SyncDisplayState::Synced => LocalizedText::key("media.status_label.synced"),
        SyncDisplayState::LocalOnly => LocalizedText::key("media.status_label.local_only"),
        SyncDisplayState::RemoteOnly => LocalizedText::key("media.status_label.remote_only"),
        SyncDisplayState::Uploading => LocalizedText::key("media.status_label.uploading"),
        SyncDisplayState::Downloading => LocalizedText::key("media.status_label.downloading"),
        SyncDisplayState::Conflict => LocalizedText::key("media.status_label.conflict"),
    }
}

#[cfg(test)]
mod value_format_tests {
    use super::*;

    const NOW: i64 = 1_700_000_000_000;

    fn ago(ms: i64) -> RelativeTimestamp {
        relative_time(NOW, NOW - ms)
    }

    #[test]
    fn relative_time_buckets_at_boundaries() {
        assert_eq!(ago(59 * 1_000), RelativeTimestamp::JustNow);
        assert_eq!(ago(MINUTE_MS), RelativeTimestamp::MinutesAgo { n: 1 });
        assert_eq!(ago(59 * MINUTE_MS), RelativeTimestamp::MinutesAgo { n: 59 });
        assert_eq!(ago(HOUR_MS), RelativeTimestamp::HoursAgo { n: 1 });
        assert_eq!(ago(DAY_MS - 1), RelativeTimestamp::HoursAgo { n: 23 });
        assert_eq!(ago(DAY_MS), RelativeTimestamp::DaysAgo { n: 1 });
        assert_eq!(ago(WEEK_MS - 1), RelativeTimestamp::DaysAgo { n: 6 });
        assert_eq!(
            ago(WEEK_MS),
            RelativeTimestamp::Absolute {
                epoch_ms: NOW - WEEK_MS
            }
        );
    }

    #[test]
    fn future_timestamp_clamps_to_just_now() {
        assert_eq!(relative_time(NOW, NOW + 5_000), RelativeTimestamp::JustNow);
    }

    #[test]
    fn short_id_truncates_at_twelve_chars() {
        // <= 12 chars: returned unchanged (empty included).
        assert_eq!(short_id(""), "");
        assert_eq!(short_id("abc"), "abc");
        assert_eq!(short_id("abcdef012345"), "abcdef012345"); // exactly 12
        // > 12 chars: first 12 + single-char ellipsis.
        assert_eq!(short_id("abcdef0123456"), "abcdef012345\u{2026}"); // 13
        let actor = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(short_id(actor), "0123456789ab\u{2026}");
    }

    /// The fingerprint is a fixed 8+`…`+8 over the canonical lowercase hex of
    /// the id — the shape both Devices-page surfaces render, pinned once so a
    /// later "shorter would read nicer" cannot narrow the collision search.
    #[test]
    fn fleet_fingerprint_keeps_eight_hex_at_each_end() {
        let mut id = [0u8; 32];
        id[..4].copy_from_slice(&[0x3f, 0x9a, 0x1b, 0x2c]);
        id[28..].copy_from_slice(&[0xc2, 0x1e, 0x9d, 0x8f]);
        assert_eq!(fleet_fingerprint(&id), "3f9a1b2c\u{2026}c21e9d8f");
        assert_eq!(fleet_fingerprint(&[0xab; 32]), "abababab\u{2026}abababab");
        assert_eq!(fleet_fingerprint(&id).chars().count(), 17);
        // The middle is elided, so two ids differing only there collide by
        // design — the edges are what the user compares.
        let mut twin = id;
        twin[10] ^= 0xff;
        assert_eq!(fleet_fingerprint(&twin), fleet_fingerprint(&id));
        let mut other = id;
        other[31] ^= 0x01;
        assert_ne!(fleet_fingerprint(&other), fleet_fingerprint(&id));
    }

    #[test]
    fn short_nest_id_elides_head_and_tail_past_twenty_chars() {
        // <= 20 chars: returned unchanged (empty included).
        assert_eq!(short_nest_id(""), "");
        assert_eq!(short_nest_id("abc"), "abc");
        let twenty = "01234567890123456789";
        assert_eq!(short_nest_id(twenty), twenty); // exactly 20
        // > 20 chars: first 8 + `…` + last 8 (middle elided).
        let twenty_one = "012345678901234567890";
        assert_eq!(short_nest_id(twenty_one), "01234567\u{2026}34567890");
        let nest_actor_id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(short_nest_id(nest_actor_id), "01234567\u{2026}89abcdef");
    }

    #[test]
    fn account_display_label_prefers_handle_else_short_id() {
        let actor = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        // Handle present and non-empty: used verbatim.
        assert_eq!(account_display_label(Some("alice"), actor), "alice");
        // No handle: falls back to the canonical short_id form (12 chars + `…`).
        assert_eq!(account_display_label(None, actor), short_id(actor));
        // Empty-string handle treated as absent, not a blank label.
        assert_eq!(account_display_label(Some(""), actor), short_id(actor));
    }

    #[test]
    fn qualified_handle_joins_a_domain_and_leaves_a_local_handle_bare() {
        // A verified foreign principal: the canonical `handle@domain`.
        assert_eq!(
            qualified_handle(Some("alice"), Some("example.com")).as_deref(),
            Some("alice@example.com")
        );
        // No domain, or an empty one: the bare handle — a local `users` row.
        assert_eq!(
            qualified_handle(Some("alice"), None).as_deref(),
            Some("alice")
        );
        assert_eq!(
            qualified_handle(Some("alice"), Some("")).as_deref(),
            Some("alice")
        );
        // No handle, or an empty one: nothing to show, whatever the domain.
        assert_eq!(qualified_handle(None, Some("example.com")), None);
        assert_eq!(qualified_handle(Some(""), Some("example.com")), None);
        assert_eq!(qualified_handle(None, None), None);
        // Fed to `account_display_label` as the handle.
        let actor = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(
            account_display_label(
                qualified_handle(Some("alice"), Some("example.com")).as_deref(),
                actor
            ),
            "alice@example.com"
        );
        assert_eq!(
            account_display_label(
                qualified_handle(None, Some("example.com")).as_deref(),
                actor
            ),
            short_id(actor)
        );
    }

    #[test]
    fn device_display_identities_render_the_code_with_any_machine_label_beside_it() {
        let a = [0x01u8; 32];
        let b = [0x02u8; 32];
        // A scrubbed/sealless row (label == "") renders the device code alone,
        // at the canonical short width — the 2026-08-02 guardian ruling: the
        // reader holds no key for the seal, so identity, not the name, renders.
        // A machine-authored plaintext label (the only plaintext the post-flip
        // column holds) is kept, beside the code rather than instead of it, so
        // every row carries something the owner does not author.
        assert_eq!(
            device_display_identities([("", &a[..]), ("fauna", &b[..])]),
            vec![
                "010101010101\u{2026}".to_string(),
                "fauna · 020202020202\u{2026}".to_string(),
            ]
        );
        // The short width is the floor, matching `short_id`, and is what a
        // list with no collision pays.
        assert_eq!(
            device_display_identities([("", &a[..])]),
            vec![short_id(&hex_full(&a))]
        );
    }

    #[test]
    fn device_display_identities_widen_only_as_far_as_the_collision_demands() {
        // Two ids agreeing on the first 12 hex characters — the shape a client
        // can choose freely, since `device_id` is not derived. The width steps
        // up until the rows separate, and no further.
        let mut near = [0x01u8; 32];
        near[7] = 0x02;
        let a = [0x01u8; 32];
        let rows = device_display_identities([("", &a[..]), ("", &near[..])]);
        assert_ne!(rows[0], rows[1], "colliding ids must not render one row");
        assert_eq!(
            rows[0], "0101010101010101\u{2026}",
            "widened by one step only"
        );

        // The worst case: ids differing in the final byte alone widen to the
        // full id rather than to a fixed guess.
        let mut last = [0x01u8; 32];
        last[31] = 0x02;
        let rows = device_display_identities([("", &a[..]), ("", &last[..])]);
        assert_ne!(rows[0], rows[1]);
        assert_eq!(rows[0], hex_full(&a), "the full id, with nothing elided");

        // Distinctness holds across the label axis too: the same machine label
        // on two devices is the ordinary self-registration case, not an attack.
        let rows = device_display_identities([("fauna", &a[..]), ("fauna", &near[..])]);
        assert_ne!(rows[0], rows[1]);
        assert!(rows.iter().all(|r| r.starts_with("fauna · ")));
    }

    #[test]
    fn author_display_label_prefers_handle_else_full_hex() {
        let author = [0x01u8, 0x23, 0xab];
        // Handle present and non-empty: used verbatim.
        assert_eq!(author_display_label(Some("alice"), &author), "alice");
        // No handle: falls back to the full hex actor id, NOT `short_id` — this
        // site shows the copyable full form (value-formatting.md § Hex id display).
        assert_eq!(author_display_label(None, &author), hex_full(&author));
        // Empty-string handle treated as absent, not a blank label.
        assert_eq!(author_display_label(Some(""), &author), hex_full(&author));
    }

    #[test]
    fn author_display_label_treats_a_whitespace_only_handle_as_absent() {
        // The drift this lift closes: linux/apple/android/windows each tested
        // only `is_empty`, so a whitespace-only handle rendered a blank row;
        // web alone trimmed. Web's richer shape is canonical (priority #4).
        let author = [0xffu8, 0x00];
        assert_eq!(
            author_display_label(Some("   "), &author),
            hex_full(&author)
        );
        assert_eq!(
            author_display_label(Some("\t\n"), &author),
            hex_full(&author)
        );
    }

    #[test]
    fn author_display_label_trims_a_padded_handle() {
        // Web returned the *trimmed* handle (`sub.handle?.trim()`), so stray
        // padding never reaches the row. That is the shape being lifted.
        let author = [0x01u8];
        assert_eq!(author_display_label(Some("  alice  "), &author), "alice");
    }

    #[test]
    fn hex_short_renders_first_four_bytes() {
        // Empty input -> empty string.
        assert_eq!(hex_short(&[]), "");
        // Fewer than 4 bytes -> all bytes, lowercase and zero-padded per byte.
        assert_eq!(hex_short(&[0x00]), "00");
        assert_eq!(hex_short(&[0x0a, 0xff]), "0aff");
        // Exactly 4 bytes -> 8 hex chars.
        assert_eq!(hex_short(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
        // More than 4 bytes -> only the first 4.
        assert_eq!(hex_short(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xab]), "01234567");
    }

    #[test]
    fn hex_full_renders_every_byte_lowercase() {
        // Empty input -> empty string.
        assert_eq!(hex_full(&[]), "");
        // Single byte -> lowercase, zero-padded.
        assert_eq!(hex_full(&[0x00]), "00");
        assert_eq!(hex_full(&[0x0a]), "0a");
        // All bytes rendered (unlike hex_short, which truncates to the first 4).
        assert_eq!(hex_full(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
        assert_eq!(
            hex_full(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xab]),
            "0123456789ab"
        );
    }

    #[test]
    fn hex_decode_roundtrips_hex_full_and_rejects_malformed_input() {
        // Empty input -> empty vec (the round-trip identity with hex_full).
        assert_eq!(hex_decode(""), Some(vec![]));
        // Mixed-case digits both accepted, same as u8::from_str_radix.
        assert_eq!(hex_decode("DEADbeef"), Some(vec![0xde, 0xad, 0xbe, 0xef]));
        assert_eq!(
            hex_decode(&hex_full(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xab])),
            Some(vec![0x01, 0x23, 0x45, 0x67, 0x89, 0xab])
        );
        // Odd length -> None, not a panic.
        assert_eq!(hex_decode("abc"), None);
        // Non-hex byte -> None.
        assert_eq!(hex_decode("zz"), None);
    }

    #[test]
    fn confidence_percent_rounds_half_up() {
        // Bounds.
        assert_eq!(confidence_percent(0), 0);
        assert_eq!(confidence_percent(1000), 100);
        // Half-up boundary: `.5 %` rounds UP, not toward zero (truncation would
        // give 0 / 92 / 99 here — the drift this shared contract prevents).
        assert_eq!(confidence_percent(4), 0);
        assert_eq!(confidence_percent(5), 1);
        assert_eq!(confidence_percent(920), 92);
        assert_eq!(confidence_percent(925), 93);
        assert_eq!(confidence_percent(994), 99);
        assert_eq!(confidence_percent(995), 100);
        // Out-of-range wire value cannot overflow (u32-widened).
        assert_eq!(confidence_percent(u16::MAX), (65535 + 5) / 10);
    }

    #[test]
    fn quota_fraction_and_percent_basic() {
        assert_eq!(quota_fraction(0, 100), 0.0);
        assert_eq!(quota_fraction(50, 100), 0.5);
        assert_eq!(quota_fraction(100, 100), 1.0);
        assert_eq!(quota_percent(0, 100), 0);
        assert_eq!(quota_percent(50, 100), 50);
        assert_eq!(quota_percent(100, 100), 100);
        // Half-up rounding on the derived percent, mirroring confidence_percent.
        assert_eq!(quota_percent(1, 3), 33);
        assert_eq!(quota_percent(2, 3), 67);
    }

    #[test]
    fn quota_fraction_guards_zero_and_negative_max() {
        // No divide-by-zero — the bug web's hand-rolled version has today.
        assert_eq!(quota_fraction(0, 0), 0.0);
        assert_eq!(quota_fraction(50, 0), 0.0);
        assert_eq!(quota_percent(50, 0), 0);
        // A malformed negative max is treated the same as zero.
        assert_eq!(quota_fraction(50, -10), 0.0);
    }

    #[test]
    fn quota_fraction_clamps_over_quota_and_negative_used() {
        // Usage that has crept past the cap never overflows the bar past full.
        assert_eq!(quota_fraction(150, 100), 1.0);
        assert_eq!(quota_percent(150, 100), 100);
        // A malformed negative used_bytes floors to zero rather than going negative.
        assert_eq!(quota_fraction(-10, 100), 0.0);
        assert_eq!(quota_percent(-10, 100), 0);
    }

    #[test]
    fn relative_time_to_localized_keys() {
        assert_eq!(
            RelativeTimestamp::JustNow.to_localized().unwrap().key,
            "time.just_now"
        );
        let mins = RelativeTimestamp::MinutesAgo { n: 5 }
            .to_localized()
            .unwrap();
        assert_eq!(mins.key, "time.minutes_ago");
        assert_eq!(mins.args.get("count").map(String::as_str), Some("5"));
        assert!(
            RelativeTimestamp::Absolute { epoch_ms: 0 }
                .to_localized()
                .is_none()
        );
    }

    #[test]
    fn byte_size_units_and_rounding() {
        let cases = [
            (0u64, "size.bytes", "0"),
            (1023, "size.bytes", "1023"),
            (1024, "size.kb", "1"),
            (1536, "size.kb", "1.5"),
            (1024 * 1024, "size.mb", "1"),
            (1024 * 1024 * 1024, "size.gb", "1"),
            (1024u64 * 1024 * 1024 * 1024, "size.tb", "1"),
        ];
        for (bytes, key, value) in cases {
            let lt = byte_size(bytes);
            assert_eq!(lt.key, key, "bytes={bytes}");
            assert_eq!(
                lt.args.get("value").map(String::as_str),
                Some(value),
                "bytes={bytes}"
            );
        }
    }

    /// Sats are the display unit, msats the wire unit — and a sub-sat tip keeps
    /// msats rather than rounding to a "0 sats" that would misreport real money
    /// as none.
    #[test]
    fn tip_amount_units_and_rounding() {
        let cases = [
            (0i64, "tips.msats", "0"),
            (1, "tips.msats", "1"),
            (500, "tips.msats", "500"),
            (999, "tips.msats", "999"),
            (1_000, "tips.sats", "1"),
            (21_000, "tips.sats", "21"),
            (21_500, "tips.sats", "21.5"),
            (1_000_000, "tips.sats", "1000"),
        ];
        for (msats, key, value) in cases {
            let lt = tip_amount(msats);
            assert_eq!(lt.key, key, "msats={msats}");
            assert_eq!(
                lt.args.get("value").map(String::as_str),
                Some(value),
                "msats={msats}"
            );
        }
    }

    /// The singular is a decision, not a formatting detail — shared so seven
    /// apps can't each ship "1 tips".
    #[test]
    fn tip_count_has_a_singular() {
        assert_eq!(tip_count(1).key, "tips.count_one");
        assert!(
            tip_count(1).args.is_empty(),
            "the singular spells its own number"
        );
        for n in [0i64, 2, 3, 11] {
            let lt = tip_count(n);
            assert_eq!(lt.key, "tips.count", "n={n}");
            assert_eq!(
                lt.args.get("count").map(String::as_str),
                Some(&*n.to_string()),
                "n={n}"
            );
        }
    }

    /// Same singular-is-a-decision shape as [`tip_count`], for the day-cell
    /// event-count tooltip.
    #[test]
    fn event_count_has_a_singular() {
        assert_eq!(event_count(1).key, "events.event_count_one");
        assert!(
            event_count(1).args.is_empty(),
            "the singular spells its own number"
        );
        for n in [0i64, 2, 3, 11] {
            let lt = event_count(n);
            assert_eq!(lt.key, "events.event_count", "n={n}");
            assert_eq!(
                lt.args.get("count").map(String::as_str),
                Some(&*n.to_string()),
                "n={n}"
            );
        }
    }

    /// Negative input clamps to zero rather than rendering "and -3 more".
    #[test]
    fn tip_more_clamps_negative_to_zero() {
        let lt = tip_more(5);
        assert_eq!(lt.key, "tips.more");
        assert_eq!(lt.args.get("count").map(String::as_str), Some("5"));
        assert_eq!(
            tip_more(-3).args.get("count").map(String::as_str),
            Some("0")
        );
    }

    #[test]
    fn duration_secs_units_and_chain() {
        // (secs, key, [(arg, value)])
        let m = duration_secs(0);
        assert_eq!(m.key, "time.uptime_m");
        assert_eq!(m.args.get("mins").map(String::as_str), Some("0"));
        assert_eq!(duration_secs(59).key, "time.uptime_m");
        assert_eq!(
            duration_secs(60).args.get("mins").map(String::as_str),
            Some("1")
        );

        let hm = duration_secs(3_661); // 1h 1m 1s
        assert_eq!(hm.key, "time.uptime_hm");
        assert_eq!(hm.args.get("hours").map(String::as_str), Some("1"));
        assert_eq!(hm.args.get("mins").map(String::as_str), Some("1"));

        let dhm = duration_secs(90_061); // 1d 1h 1m 1s
        assert_eq!(dhm.key, "time.uptime_dhm");
        assert_eq!(dhm.args.get("days").map(String::as_str), Some("1"));
        assert_eq!(dhm.args.get("hours").map(String::as_str), Some("1"));
        assert_eq!(dhm.args.get("mins").map(String::as_str), Some("1"));

        // Days present but zero hours/mins still show the full chain.
        let day = duration_secs(86_400);
        assert_eq!(day.key, "time.uptime_dhm");
        assert_eq!(day.args.get("hours").map(String::as_str), Some("0"));
        assert_eq!(day.args.get("mins").map(String::as_str), Some("0"));
    }

    #[test]
    fn grace_countdown_elapsed_deadline_returns_none() {
        assert_eq!(grace_countdown(1_000, 1_000), None, "exactly at deadline");
        assert_eq!(grace_countdown(1_000, 2_000), None, "past deadline");
    }

    #[test]
    fn grace_countdown_sub_hour_remaining_shows_zero_hours() {
        let lt = grace_countdown(30 * 60_000, 0).unwrap(); // 30 min left
        assert_eq!(lt.key, "time.countdown_h");
        assert_eq!(lt.args.get("hours").map(String::as_str), Some("0"));
    }

    #[test]
    fn grace_countdown_hours_only_under_a_day() {
        let lt = grace_countdown(5 * 3_600_000, 0).unwrap(); // 5h left
        assert_eq!(lt.key, "time.countdown_h");
        assert_eq!(lt.args.get("hours").map(String::as_str), Some("5"));
        assert!(!lt.args.contains_key("days"));
    }

    #[test]
    fn grace_countdown_days_and_hours_chain() {
        let lt = grace_countdown((2 * 24 + 3) * 3_600_000, 0).unwrap(); // 2d3h left
        assert_eq!(lt.key, "time.countdown_dh");
        assert_eq!(lt.args.get("days").map(String::as_str), Some("2"));
        assert_eq!(lt.args.get("hours").map(String::as_str), Some("3"));
    }

    #[test]
    fn grace_countdown_exact_day_boundary_shows_zero_hours() {
        let lt = grace_countdown(24 * 3_600_000, 0).unwrap();
        assert_eq!(lt.key, "time.countdown_dh");
        assert_eq!(lt.args.get("days").map(String::as_str), Some("1"));
        assert_eq!(lt.args.get("hours").map(String::as_str), Some("0"));
    }

    #[test]
    fn relative_time_display_flattens() {
        // A relative bucket → localized key, no absolute epoch.
        let recent = relative_time_display(NOW, NOW - 2 * HOUR_MS);
        assert_eq!(recent.localized.as_ref().unwrap().key, "time.hours_ago");
        assert_eq!(recent.absolute_epoch_ms, None);
        // ≥7d → no key, carries the absolute epoch for native date formatting.
        let old = relative_time_display(NOW, NOW - WEEK_MS);
        assert!(old.localized.is_none());
        assert_eq!(old.absolute_epoch_ms, Some(NOW - WEEK_MS));
    }

    // NOW = 2023-11-14 (a Tuesday) 22:13:20 UTC; UTC day index 19675.
    #[test]
    fn conversation_timestamp_today_is_local_clock() {
        // Same instant (UTC) → today, local wall-clock 22:13.
        assert_eq!(
            conversation_timestamp(NOW, NOW, 0),
            ConversationTimestamp::Today {
                hour: 22,
                minute: 13
            }
        );
        // Earlier the same calendar day stays "today".
        assert_eq!(
            conversation_timestamp(NOW, NOW - 2 * HOUR_MS, 0),
            ConversationTimestamp::Today {
                hour: 20,
                minute: 13
            }
        );
    }

    #[test]
    fn conversation_timestamp_offset_shifts_the_local_clock_and_day() {
        // UTC+2: 22:13 UTC is 00:13 the *next* local day → still "today" vs a
        // now that is also shifted by the same offset.
        assert_eq!(
            conversation_timestamp(NOW, NOW, 7200),
            ConversationTimestamp::Today {
                hour: 0,
                minute: 13
            }
        );
        // UTC-2: 22:13 UTC is 20:13 local.
        assert_eq!(
            conversation_timestamp(NOW, NOW, -7200),
            ConversationTimestamp::Today {
                hour: 20,
                minute: 13
            }
        );
    }

    #[test]
    fn conversation_timestamp_yesterday_is_calendar_based_not_24h() {
        // Exactly a day ago → yesterday.
        assert_eq!(
            conversation_timestamp(NOW, NOW - DAY_MS, 0),
            ConversationTimestamp::Yesterday
        );
        // 23:50 on the previous calendar day is ~22 h ago but still "yesterday".
        let prev_day_late = (19674 * DAY_MS) + 23 * HOUR_MS + 50 * MINUTE_MS;
        assert_eq!(
            conversation_timestamp(NOW, prev_day_late, 0),
            ConversationTimestamp::Yesterday
        );
    }

    #[test]
    fn conversation_timestamp_weekday_for_two_to_six_days() {
        // 2 days ago = 2023-11-12, a Sunday (index 6).
        assert_eq!(
            conversation_timestamp(NOW, NOW - 2 * DAY_MS, 0),
            ConversationTimestamp::Weekday { index: 6 }
        );
        // 6 days ago = 2023-11-08, a Wednesday (index 2) — last day before "older".
        assert_eq!(
            conversation_timestamp(NOW, NOW - 6 * DAY_MS, 0),
            ConversationTimestamp::Weekday { index: 2 }
        );
    }

    #[test]
    fn conversation_timestamp_older_at_seven_days() {
        assert_eq!(
            conversation_timestamp(NOW, NOW - 7 * DAY_MS, 0),
            ConversationTimestamp::Older {
                epoch_ms: NOW - 7 * DAY_MS
            }
        );
    }

    #[test]
    fn conversation_timestamp_to_localized_keys() {
        assert!(
            ConversationTimestamp::Today { hour: 9, minute: 5 }
                .to_localized()
                .is_none()
        );
        assert_eq!(
            ConversationTimestamp::Yesterday.to_localized().unwrap().key,
            "time.yesterday"
        );
        assert_eq!(
            ConversationTimestamp::Weekday { index: 0 }
                .to_localized()
                .unwrap()
                .key,
            "time.weekday_mon"
        );
        assert_eq!(
            ConversationTimestamp::Weekday { index: 6 }
                .to_localized()
                .unwrap()
                .key,
            "time.weekday_sun"
        );
        assert!(
            ConversationTimestamp::Older { epoch_ms: 0 }
                .to_localized()
                .is_none()
        );
    }

    #[test]
    fn conversation_timestamp_display_flattens() {
        // Today → 24h clock string, nothing else.
        let today = conversation_timestamp_display(NOW, NOW, 0);
        assert_eq!(today.clock.as_deref(), Some("22:13"));
        assert!(today.localized.is_none() && today.absolute_epoch_ms.is_none());
        // Yesterday → localized key.
        let y = conversation_timestamp_display(NOW, NOW - DAY_MS, 0);
        assert_eq!(y.localized.as_ref().unwrap().key, "time.yesterday");
        assert!(y.clock.is_none() && y.absolute_epoch_ms.is_none());
        // Weekday → localized key.
        let w = conversation_timestamp_display(NOW, NOW - 2 * DAY_MS, 0);
        assert_eq!(w.localized.as_ref().unwrap().key, "time.weekday_sun");
        assert!(w.clock.is_none());
        // Older → absolute epoch for native date formatting.
        let old = conversation_timestamp_display(NOW, NOW - 7 * DAY_MS, 0);
        assert_eq!(old.absolute_epoch_ms, Some(NOW - 7 * DAY_MS));
        assert!(old.clock.is_none() && old.localized.is_none());
    }

    #[test]
    fn url_host_extracts_host() {
        assert_eq!(
            url_host("https://backup.example.com/"),
            "backup.example.com"
        );
        assert_eq!(
            url_host("https://backup.example.com:8443"),
            "backup.example.com"
        );
        assert_eq!(url_host("http://10.0.0.5:3000/x"), "10.0.0.5");
        // No scheme → the input is already a bare host.
        assert_eq!(url_host("bare-host"), "bare-host");
        // No host can be isolated → fall back to the original string.
        assert_eq!(url_host(""), "");
        assert_eq!(url_host("https://"), "https://");
    }

    #[test]
    fn url_host_opt_extracts_host() {
        assert_eq!(
            url_host_opt("wss://example.com:8080/path").as_deref(),
            Some("example.com")
        );
        // No scheme → the input is already a bare host.
        assert_eq!(url_host_opt("example.com").as_deref(), Some("example.com"));
    }

    #[test]
    fn url_host_opt_does_not_show_userinfo_as_the_domain() {
        // The display spoof: this label is what every app shows as "where
        // this link goes". Splitting on `:` before looking for an `@` made a
        // link to `evil.com` render under a trusted domain's name.
        assert_eq!(
            url_host_opt("https://example.com:x@evil.com/").as_deref(),
            Some("evil.com")
        );
        assert_eq!(
            url_host_opt("https://example.com@evil.com/").as_deref(),
            Some("evil.com")
        );
        assert_eq!(
            url_host_opt("https://user:pass@example.com:8443/x").as_deref(),
            Some("example.com")
        );
        // A `@` later in the path is not userinfo and must not move the host.
        assert_eq!(
            url_host_opt("https://example.com/u/alice@host").as_deref(),
            Some("example.com")
        );
    }

    #[test]
    fn url_host_opt_is_none_when_no_host_can_be_isolated() {
        // The empty-host cases `url_host`'s echo fallback masks — this is
        // exactly why `nest_host` (apps/fauna-tui/src/admin/mod.rs) needs the
        // `Option` contract instead: a naive `if url_host(u).contains("://")`
        // adapter over `url_host` cannot tell these apart from a genuine bare
        // host, because the echoed original happens not to contain "://".
        assert_eq!(url_host_opt(""), None);
        assert_eq!(url_host_opt(":"), None);
        assert_eq!(url_host_opt("http://"), None);
        assert_eq!(url_host_opt("wss://"), None);
    }

    #[test]
    fn url_host_opt_stops_at_every_authority_terminator() {
        // PROBE-613, second test: the link-preview chip's display spoof — a
        // link that did not stop at the right terminator rendered the
        // userinfo-lookalike host instead of the one every WHATWG parser (and
        // the dialer) actually reaches . Shared cases
        // so a narrower `authority_len` reds this alongside the other five
        // callers .
        for &(url, expected_host) in crate::web::AUTHORITY_TERMINATOR_CASES {
            assert_eq!(url_host_opt(url).as_deref(), Some(expected_host), "{url}");
        }
    }

    #[test]
    fn url_host_and_url_host_opt_agree_everywhere_but_the_none_case() {
        for input in [
            "wss://example.com:8080/path",
            "example.com",
            "",
            ":",
            "http://",
            "wss://",
        ] {
            match url_host_opt(input) {
                Some(host) => assert_eq!(url_host(input), host, "input {input:?}"),
                None => assert_eq!(url_host(input), input, "input {input:?} echoes original"),
            }
        }
    }

    #[test]
    fn parse_port_accepts_valid_ports() {
        assert_eq!(parse_port("8443"), Some(8443));
        assert_eq!(parse_port("1"), Some(1)); // lower bound
        assert_eq!(parse_port("65535"), Some(65535)); // upper bound
        assert_eq!(parse_port("  443  "), Some(443)); // surrounding whitespace trimmed
        // A leading '+' is a valid unsigned literal — every app's parser
        // (C# ushort.TryParse, Kotlin toIntOrNull, Swift Int()) accepts it too.
        assert_eq!(parse_port("+443"), Some(443));
    }

    #[test]
    fn parse_port_rejects_invalid_ports() {
        assert_eq!(parse_port("0"), None); // 0 is not a bindable listener port
        assert_eq!(parse_port("65536"), None); // above u16 max
        assert_eq!(parse_port("99999"), None);
        assert_eq!(parse_port(""), None);
        assert_eq!(parse_port("abc"), None);
        assert_eq!(parse_port("-1"), None); // negative
        assert_eq!(parse_port("84.43"), None); // fractional
        assert_eq!(parse_port("8 443"), None); // interior whitespace
    }

    #[test]
    fn parse_cap_accepts_and_clamps_non_negative() {
        // A valid non-negative i64 cap. Unlike a port, `0` is valid here (an
        // explicit "no allowance").
        assert_eq!(parse_cap("100"), Some(100));
        assert_eq!(parse_cap("0"), Some(0));
        assert_eq!(parse_cap("  500  "), Some(500)); // surrounding whitespace trimmed
        // A leading '+' is a valid signed-int literal — every app's parser
        // (Rust parse, Kotlin toLongOrNull, C# long.TryParse) accepts it.
        assert_eq!(parse_cap("+42"), Some(42));
        assert_eq!(parse_cap("9223372036854775807"), Some(i64::MAX)); // i64::MAX round-trips
        // A negative value CLAMPS to 0 (matching android coerceAtLeast(0) / windows
        // Math.Max(0, v) / linux .max(0)) — it is not a fallback, so consumed as
        // `parse_cap(text).unwrap_or(prev)` it yields 0, never `prev`.
        assert_eq!(parse_cap("-5"), Some(0));
    }

    #[test]
    fn parse_cap_rejects_unparseable() {
        // Empty / whitespace-only / non-numeric / fractional / out-of-range → None,
        // so the caller keeps the persisted `prev` (no silent zeroing).
        assert_eq!(parse_cap(""), None);
        assert_eq!(parse_cap("   "), None);
        assert_eq!(parse_cap("abc"), None);
        assert_eq!(parse_cap("1.5"), None); // fractional
        assert_eq!(parse_cap("9223372036854775808"), None); // i64::MAX + 1 overflows
        assert_eq!(parse_cap("8 443"), None); // interior whitespace
    }

    #[test]
    fn total_pages_ceil_divides_and_floors_to_one() {
        assert_eq!(total_pages(0, 50), 1); // empty list — still "1 / 1", never "1 / 0"
        assert_eq!(total_pages(1, 50), 1);
        assert_eq!(total_pages(50, 50), 1); // exact multiple — no phantom empty page
        assert_eq!(total_pages(51, 50), 2); // one over a multiple — ceil, not floor
        assert_eq!(total_pages(100, 50), 2);
        assert_eq!(total_pages(101, 50), 3);
    }

    #[test]
    fn current_page_is_one_based_from_offset() {
        assert_eq!(current_page(0, 50), 1);
        assert_eq!(current_page(49, 50), 1);
        assert_eq!(current_page(50, 50), 2);
        assert_eq!(current_page(100, 50), 3);
    }

    #[test]
    fn next_page_offset_advances_one_page_or_stops_at_the_end() {
        assert_eq!(next_page_offset(0, 100, 50), Some(50)); // more rows past the page
        assert_eq!(next_page_offset(50, 100, 50), None); // offset+page_size == total: last page
        assert_eq!(next_page_offset(0, 30, 50), None); // one page holds everything
        assert_eq!(next_page_offset(0, 0, 50), None); // empty list
        assert_eq!(next_page_offset(0, 51, 50), Some(50)); // one row spills onto page 2
    }

    #[test]
    fn prev_page_offset_retreats_one_page_or_stops_at_the_start() {
        assert_eq!(prev_page_offset(50, 50), Some(0));
        assert_eq!(prev_page_offset(100, 50), Some(50));
        assert_eq!(prev_page_offset(0, 50), None); // already on page 1
        assert_eq!(prev_page_offset(30, 50), Some(0)); // a stale offset still floors to 0, never negative
    }

    #[test]
    fn parse_count_accepts_non_negative_u32() {
        assert_eq!(parse_count("3600"), Some(3600));
        // `0` is a valid knob value (e.g. a disabled rate limit) — like a cap, not a port.
        assert_eq!(parse_count("0"), Some(0));
        assert_eq!(parse_count("  42  "), Some(42)); // surrounding whitespace trimmed
        // A leading '+' is a valid unsigned literal — every app's native parser
        // accepts it, and the canonical rule converges on accepting it (web previously
        // rejected it via `^\d+$`).
        assert_eq!(parse_count("+7"), Some(7));
        assert_eq!(parse_count("4294967295"), Some(u32::MAX)); // u32::MAX round-trips
    }

    #[test]
    fn parse_count_rejects_unparseable() {
        // Empty / whitespace-only / non-numeric / negative / fractional / overflow → None,
        // so the caller keeps the persisted `prev` (no silent zeroing).
        assert_eq!(parse_count(""), None);
        assert_eq!(parse_count("   "), None);
        assert_eq!(parse_count("abc"), None);
        assert_eq!(parse_count("-1"), None); // negative (an unsigned knob)
        assert_eq!(parse_count("1.5"), None); // fractional
        assert_eq!(parse_count("4294967296"), None); // u32::MAX + 1 overflows
        assert_eq!(parse_count("3 600"), None); // interior whitespace
    }

    #[test]
    fn parse_count_u64_accepts_large_byte_ceilings() {
        // The IMAP storage ceiling routinely exceeds u32 (10 GiB here).
        assert_eq!(parse_count_u64("10737418240"), Some(10_737_418_240));
        assert_eq!(parse_count_u64("0"), Some(0));
        assert_eq!(parse_count_u64("  500  "), Some(500)); // surrounding whitespace trimmed
        assert_eq!(parse_count_u64("+8"), Some(8)); // leading '+' accepted
        assert_eq!(parse_count_u64("18446744073709551615"), Some(u64::MAX)); // u64::MAX round-trips
    }

    #[test]
    fn parse_count_u64_rejects_unparseable() {
        assert_eq!(parse_count_u64(""), None);
        assert_eq!(parse_count_u64("abc"), None);
        assert_eq!(parse_count_u64("-1"), None); // negative
        assert_eq!(parse_count_u64("1.5"), None); // fractional
        assert_eq!(parse_count_u64("18446744073709551616"), None); // u64::MAX + 1 overflows
    }

    #[test]
    fn parse_count_i64_accepts_non_negative_signed() {
        // The per-alias `rate_limit_per_hour` override is a signed-i64 wire column but
        // a non-negative cap in meaning (null = unlimited, 0 = block all).
        assert_eq!(parse_count_i64("100"), Some(100));
        assert_eq!(parse_count_i64("0"), Some(0)); // an explicit "block all"
        assert_eq!(parse_count_i64("  500  "), Some(500)); // surrounding whitespace trimmed
        // A leading '+' is a valid signed-int literal, as in the rest of the family.
        assert_eq!(parse_count_i64("+42"), Some(42));
        assert_eq!(parse_count_i64("9223372036854775807"), Some(i64::MAX)); // i64::MAX round-trips
    }

    #[test]
    fn parse_count_i64_rejects_unparseable_and_negative() {
        // Empty / whitespace-only / non-numeric / fractional / overflow → None.
        assert_eq!(parse_count_i64(""), None);
        assert_eq!(parse_count_i64("   "), None);
        assert_eq!(parse_count_i64("abc"), None);
        assert_eq!(parse_count_i64("1.5"), None); // fractional (resolves web's Math.floor drift)
        assert_eq!(parse_count_i64("9223372036854775808"), None); // i64::MAX + 1 overflows
        assert_eq!(parse_count_i64("8 443"), None); // interior whitespace
        // A negative value is meaningless for a non-negative cap → None (every
        // count-family member yields a non-negative result; matches web's deliberate
        // `>= 0` guard, resolves the natives' incidental accept-negative).
        assert_eq!(parse_count_i64("-5"), None);
    }

    #[test]
    fn parse_weight_permille_scales_decimal_multiplier() {
        // "1.0" is the baseline weight the editors pre-fill.
        assert_eq!(parse_weight_permille("1.0"), 1000);
        assert_eq!(parse_weight_permille("2.0"), 2000);
        assert_eq!(parse_weight_permille("1.5"), 1500);
        assert_eq!(parse_weight_permille("  2.5  "), 2500); // surrounding whitespace trimmed
        assert_eq!(parse_weight_permille("0"), 0);
        // A negative weight is a designed case, not an error: a strong-negative
        // factor sinks an item below any rendered page (`docs/goal/ui/feed.md`
        // § Frame reconciliation — filtering falls out of ordering).
        assert_eq!(parse_weight_permille("-1"), -1000);
        assert_eq!(parse_weight_permille("-2.5"), -2500);
    }

    #[test]
    fn parse_weight_permille_rounds_half_away_from_zero() {
        // Pins the midpoint rule against BOTH per-app drifts this fn unifies:
        // C#'s `Math.Round` default (banker's/to-even → 2 and -2) and JS's
        // `Math.round` (half-up toward +∞ → 3 and -2).
        assert_eq!(parse_weight_permille("0.0025"), 3);
        assert_eq!(parse_weight_permille("-0.0025"), -3);
    }

    #[test]
    fn parse_weight_permille_falls_back_to_baseline_on_bad_input() {
        // An unparseable entry falls back to the 1.0 baseline rather than
        // silently dropping the caller's add (the linux editor's behavior).
        assert_eq!(parse_weight_permille(""), 1000);
        assert_eq!(parse_weight_permille("   "), 1000);
        assert_eq!(parse_weight_permille("abc"), 1000);
        // Strict, whole-string parse — resolves web's lenient `parseFloat("2abc") == 2`.
        assert_eq!(parse_weight_permille("2abc"), 1000);
        // Rust parses these to non-finite floats; `as i64` would saturate, so the
        // finite-guard sends them to the baseline instead.
        assert_eq!(parse_weight_permille("inf"), 1000);
        assert_eq!(parse_weight_permille("-inf"), 1000);
        assert_eq!(parse_weight_permille("NaN"), 1000);
    }

    #[test]
    fn format_weight_permille_strips_trailing_zeros() {
        // The baseline weight and other whole multipliers render with no
        // decimal point at all, not "1.00" / "2.00".
        assert_eq!(format_weight_permille(1000), "1");
        assert_eq!(format_weight_permille(2000), "2");
        assert_eq!(format_weight_permille(0), "0");
    }

    #[test]
    fn format_weight_permille_keeps_significant_fraction_digits() {
        assert_eq!(format_weight_permille(1500), "1.5");
        assert_eq!(format_weight_permille(-2500), "-2.5");
        // Rounds to 2 decimal places, matching the richest of the per-app
        // hand-rolls this unifies (web/windows kept 2 d.p.; apple's fixed 1
        // d.p. loses precision `parse_weight_permille` itself preserves).
        assert_eq!(format_weight_permille(1234), "1.23");
    }

    #[test]
    fn format_weight_permille_round_trips_the_baseline() {
        // Parsing the editor's pre-filled "1.0" then formatting it back must
        // reproduce a clean baseline, not "1.0" or "1.00".
        assert_eq!(format_weight_permille(parse_weight_permille("1.0")), "1");
    }

    #[test]
    fn backup_destination_label_prefers_display_name() {
        assert_eq!(
            backup_destination_label(Some("Aunt's nest"), "https://nas.example.com:8443"),
            "Aunt's nest"
        );
        // Empty / absent display name → the URL host.
        assert_eq!(
            backup_destination_label(Some(""), "https://nas.example.com:8443"),
            "nas.example.com"
        );
        assert_eq!(
            backup_destination_label(None, "https://nas.example.com/"),
            "nas.example.com"
        );
    }

    #[test]
    fn backup_last_upload_label_is_never_when_absent_or_zero() {
        // Absent status/timestamp AND a zero timestamp both mean "never" — the
        // richest prior guard (linux/web checked `secs > 0`; apple/android
        // treated any non-null as real and would render the 1970 epoch).
        for t in [None, Some(0)] {
            let d = backup_last_upload_label(t, 1_700_000_000_000);
            assert_eq!(
                d.label,
                LocalizedText::key("backups.backup_destination_last_upload_never")
            );
            assert!(d.when.is_none());
        }
    }

    #[test]
    fn backup_last_upload_label_wraps_relative_time_as_when() {
        let now_ms: i64 = 1_700_000_000_000;
        let then_secs = now_ms / 1_000 - 2 * 60; // two minutes ago, in unix SECONDS
        let d = backup_last_upload_label(Some(then_secs as u64), now_ms);
        assert_eq!(
            d.label,
            LocalizedText::key("backups.backup_destination_last_upload")
        );
        // `when` is the seconds→ms-converted shared relative-time display the
        // client resolves and substitutes as the label's `{when}` arg.
        let when = d.when.expect("a real timestamp carries a `when`");
        assert_eq!(when, relative_time_display(now_ms, then_secs * 1_000));
        assert_eq!(
            when.localized,
            Some(LocalizedText::key_arg("time.minutes_ago", "count", "2"))
        );
    }

    /// "Never" is the honest reading of a destination that has never passed an
    /// audit — including a freshly enrolled one, which is healthy. The same
    /// `> 0` epoch guard as the upload twin, so a zero stamp never renders 1970.
    #[test]
    fn a_custodian_that_never_self_audited_renders_absence_not_a_verdict() {
        // The nest refuses to invent a verdict for a custodian that has not
        // reported; the app must not invent one either.
        for t in [None, Some(0)] {
            let d = backup_self_audit_label(t, 1_700_000_000_000);
            assert_eq!(
                d.label.key, "backups.backup_destination_last_self_audit_never",
                "absent/zero self-audit must read as not-yet, not as a pass"
            );
            assert!(d.when.is_none());
        }
    }

    #[test]
    fn a_self_audit_stamp_never_borrows_the_owner_side_wording() {
        // The two doors answer the same question from opposite sides of the
        // trust line: one is what this client verified, the other is what the
        // destination says about itself. Sharing a key would let a self-report
        // wear the words of an independent verification.
        let selfish = backup_self_audit_label(Some(1_699_999_000), 1_700_000_000_000);
        let owner = backup_last_audit_label(Some(1_699_999_000), 1_700_000_000_000);
        assert_ne!(selfish.label.key, owner.label.key);
        assert!(selfish.when.is_some() && owner.when.is_some());
    }

    #[test]
    fn only_a_reported_failure_is_loud() {
        assert!(backup_self_audit_is_alerting(Some(
            crate::data::AUDIT_STATE_FAILED
        )));
        // Silence (a nest row not yet audited) is a normal state, and an unrecognised
        // value is one this client cannot read as a failure — a false
        // data-loss alarm is the worst way to be wrong here.
        for quiet in [
            None,
            Some(crate::data::AUDIT_STATE_OK),
            Some("something-newer"),
        ] {
            assert!(
                !backup_self_audit_is_alerting(quiet),
                "must stay quiet for {quiet:?}"
            );
        }
    }

    #[test]
    fn backup_last_audit_label_is_never_when_absent_or_zero() {
        for t in [None, Some(0)] {
            let d = backup_last_audit_label(t, 1_700_000_000_000);
            assert_eq!(
                d.label,
                LocalizedText::key("backups.backup_destination_last_audit_never")
            );
            assert!(d.when.is_none());
        }
    }

    #[test]
    fn backup_last_audit_label_wraps_relative_time_as_when() {
        let now_ms: i64 = 1_700_000_000_000;
        let then_secs = now_ms / 1_000 - 2 * 60 * 60; // two hours ago, in SECONDS
        let d = backup_last_audit_label(Some(then_secs as u64), now_ms);
        assert_eq!(
            d.label,
            LocalizedText::key("backups.backup_destination_last_audit")
        );
        let when = d.when.expect("a real timestamp carries a `when`");
        assert_eq!(when, relative_time_display(now_ms, then_secs * 1_000));
        assert_eq!(
            when.localized,
            Some(LocalizedText::key_arg("time.hours_ago", "count", "2"))
        );
    }

    /// The banner is indexed — one per failing destination — so every reason
    /// must name the destination, or a user with two destinations cannot tell
    /// which backup is broken.
    #[test]
    fn every_audit_alert_reason_names_the_destination() {
        let reasons = [
            BackupAuditAlertReason::Freshness {
                lag_secs: 5 * 24 * 60 * 60,
            },
            BackupAuditAlertReason::Inclusion {
                missing: 2,
                sampled: 16,
            },
            BackupAuditAlertReason::Overdue {
                since_secs: 9 * 24 * 60 * 60,
            },
        ];
        let mut keys = std::collections::BTreeSet::new();
        for reason in reasons {
            let text = backup_audit_alert_label(reason, "backup.example");
            assert_eq!(
                text.args.get("destination").map(String::as_str),
                Some("backup.example"),
                "{reason:?} must name its destination"
            );
            keys.insert(text.key);
        }
        assert_eq!(keys.len(), 3, "each reason gets its own distinct key");
    }

    #[test]
    fn parse_byte_size_reads_what_people_actually_type() {
        let gib = 1024u64 * 1024 * 1024;
        for (input, want) in [
            ("50 GB", 50 * gib),
            ("50GB", 50 * gib),
            ("50 gb", 50 * gib),
            ("50 GiB", 50 * gib),
            ("50g", 50 * gib),
            ("500 MB", 500 * 1024 * 1024),
            ("1.5 TB", gib * 1024 * 3 / 2),
            ("1,5 TB", gib * 1024 * 3 / 2),
            ("1024", 1024),
            ("  2 tb  ", 2 * 1024 * gib),
        ] {
            assert_eq!(parse_byte_size(input), Some(want), "parsing {input:?}");
        }
    }

    #[test]
    fn parse_byte_size_refuses_rather_than_guesses() {
        // A cap the shell cannot read must become a refusal the user sees, never
        // a substituted default — a wrong cap silently fills the device's disk.
        for input in ["", "   ", "lots", "-5 GB", "5 bananas", "GB", "1e400 GB"] {
            assert_eq!(parse_byte_size(input), None, "must refuse {input:?}");
        }
    }

    #[test]
    fn parse_byte_size_round_trips_through_byte_size() {
        // The cap is re-rendered into the same input on every repaint, so a
        // value that drifts through one round trip drifts on every one.
        for bytes in [512u64, 1024, 50 * 1024 * 1024, 50 * 1024 * 1024 * 1024] {
            let rendered = byte_size(bytes);
            let value = rendered.args.get("value").expect("byte_size carries value");
            let unit_suffix = match rendered.key.as_str() {
                "size.bytes" => "",
                "size.kb" => "KB",
                "size.mb" => "MB",
                "size.gb" => "GB",
                "size.tb" => "TB",
                other => panic!("unexpected unit key {other}"),
            };
            let typed = format!("{value} {unit_suffix}");
            assert_eq!(
                parse_byte_size(&typed),
                Some(bytes),
                "{bytes} rendered as {typed:?} must parse back unchanged"
            );
        }
    }

    #[test]
    fn destination_kind_badge_distinguishes_the_two_built_kinds() {
        let nest = backup_destination_kind_label(crate::data::DESTINATION_KIND_NEST);
        let device = backup_destination_kind_label(crate::data::DESTINATION_KIND_CLIENT_DEVICE);
        assert_ne!(
            nest.key, device.key,
            "a client custodian must never render as an off-site nest — the badge \
             is the whole durability distinction (backups.md § Durability + labeling)"
        );
        assert!(nest.args.is_empty() && device.args.is_empty());
    }

    #[test]
    fn an_unknown_destination_kind_shows_what_it_is() {
        // A newer client wrote a kind this build cannot drive. The user needs to
        // see *which* one, not a generic word — the `Inert` arm's whole point.
        let text = backup_destination_kind_label("s3");
        assert_eq!(text.args.get("kind").map(String::as_str), Some("s3"));
        assert_ne!(
            text.key,
            backup_destination_kind_label(crate::data::DESTINATION_KIND_NEST).key
        );
    }

    #[test]
    fn usage_reads_cap_reached_from_the_state_never_from_held_vs_cap() {
        // The load-bearing case: a pass that stopped at its cap ends BELOW the
        // cap, so `held < cap` while the state says reached. Inferring from the
        // two numbers would render this as healthy-with-room — for a backup that
        // has silently stopped advancing.
        let stopped_below_cap = backup_usage_label(
            Some(30_000_000_000),
            Some(50_000_000_000),
            Some(crate::data::CAP_STATE_REACHED),
        );
        let genuinely_ok = backup_usage_label(
            Some(30_000_000_000),
            Some(50_000_000_000),
            Some(crate::data::CAP_STATE_OK),
        );
        assert_ne!(
            stopped_below_cap.label.key, genuinely_ok.label.key,
            "identical byte counts, opposite verdicts — the state is the only input"
        );
        assert!(stopped_below_cap.held.is_some() && stopped_below_cap.cap.is_some());
    }

    #[test]
    fn usage_separates_never_checked_in_from_uncapped_from_capped() {
        let never = backup_usage_label(None, Some(1_000), Some(crate::data::CAP_STATE_OK));
        let uncapped = backup_usage_label(Some(1_000), None, Some(crate::data::CAP_STATE_OK));
        let capped = backup_usage_label(Some(1_000), Some(2_000), Some(crate::data::CAP_STATE_OK));

        // "Nothing held yet" is not "0 bytes held": only the second asserts the
        // device actually reported.
        assert!(never.held.is_none() && never.cap.is_none());
        // Uncapped is a real configuration, so it gets its own sentence rather
        // than an "of {cap}" with an empty cap.
        assert!(uncapped.held.is_some() && uncapped.cap.is_none());
        assert!(capped.held.is_some() && capped.cap.is_some());

        let keys = std::collections::BTreeSet::from([
            never.label.key,
            uncapped.label.key,
            capped.label.key,
        ]);
        assert_eq!(keys.len(), 3, "each state gets its own distinct key");
    }

    #[test]
    fn audit_alert_durations_render_as_whole_days() {
        let day = 24 * 60 * 60;
        let freshness = backup_audit_alert_label(
            BackupAuditAlertReason::Freshness {
                lag_secs: 5 * day + 3600,
            },
            "d",
        );
        assert_eq!(freshness.key, "backups.backup_audit_alert_freshness");
        assert_eq!(freshness.args.get("days").map(String::as_str), Some("5"));

        let overdue = backup_audit_alert_label(
            BackupAuditAlertReason::Overdue {
                since_secs: 9 * day,
            },
            "d",
        );
        assert_eq!(overdue.key, "backups.backup_audit_alert_overdue");
        assert_eq!(overdue.args.get("days").map(String::as_str), Some("9"));

        // Floors to 1, never 0: a sub-day remainder still happened, and
        // "0 days behind" reads as "not behind".
        let sub_day =
            backup_audit_alert_label(BackupAuditAlertReason::Freshness { lag_secs: 3600 }, "d");
        assert_eq!(sub_day.args.get("days").map(String::as_str), Some("1"));
    }

    /// The recovery notice counts down while a deadline exists, and says
    /// "until recovered" — its own string, no day count — when none does.
    #[test]
    fn source_regressed_label_has_a_deadline_form_and_an_until_recovered_form() {
        let day = 24 * 60 * 60;
        let dated = backup_audit_alert_label(
            BackupAuditAlertReason::SourceRegressed {
                left_secs: Some(4 * day + 60),
            },
            "d",
        );
        assert_eq!(dated.key, "backups.backup_audit_alert_source_regressed");
        assert_eq!(dated.args.get("days").map(String::as_str), Some("4"));

        let open_ended = backup_audit_alert_label(
            BackupAuditAlertReason::SourceRegressed { left_secs: None },
            "d",
        );
        assert_eq!(
            open_ended.key,
            "backups.backup_audit_alert_source_regressed_until_recovered"
        );
        assert_eq!(open_ended.args.get("days"), None);
        assert_eq!(
            open_ended.args.get("destination").map(String::as_str),
            Some("d")
        );
    }

    #[test]
    fn audit_alert_inclusion_reports_both_counts() {
        let text = backup_audit_alert_label(
            BackupAuditAlertReason::Inclusion {
                missing: 3,
                sampled: 16,
            },
            "backup.example",
        );
        assert_eq!(text.key, "backups.backup_audit_alert_inclusion");
        assert_eq!(text.args.get("missing").map(String::as_str), Some("3"));
        assert_eq!(text.args.get("sampled").map(String::as_str), Some("16"));
    }

    #[test]
    fn backup_backlog_label_counts_and_defaults_absent_to_zero() {
        assert_eq!(
            backup_backlog_label(Some(7)),
            LocalizedText::key_arg("backups.backup_destination_backlog", "count", "7")
        );
        // No status read yet ⇒ "0 queued" (every app's baseline today).
        assert_eq!(
            backup_backlog_label(None),
            LocalizedText::key_arg("backups.backup_destination_backlog", "count", "0")
        );
    }

    #[test]
    fn contact_status_label_maps_every_status() {
        // Every canonical relationship status (`fauna_core::data::ContactStatus`;
        // contacts.md § Encryption at rest) carries its `common.*` i18n key.
        assert_eq!(contact_status_label("pending").key, "common.pending");
        assert_eq!(contact_status_label("accepted").key, "common.accepted");
        assert_eq!(contact_status_label("confirmed").key, "common.confirmed");
        assert_eq!(contact_status_label("blocked").key, "common.blocked");
        // An unknown status (e.g. linux's outbound-knock `sent`, or a future
        // status) falls back to its capitalized form rendered verbatim (no i18n
        // entry) — the richest prior behavior, matching `rsvp_status_label`, and
        // strictly better than windows' lossy `_ => "Unknown"`.
        let fallback = contact_status_label("sent");
        assert_eq!(fallback.key, "Sent");
        assert_eq!(fallback.resolve(|_| None::<&str>), "Sent");
        // An empty status resolves to empty (no spurious capitalization).
        assert_eq!(contact_status_label("").resolve(|_| None::<&str>), "");
    }

    #[test]
    fn thread_label_display_falls_back_for_blank_labels() {
        // A non-empty label rides verbatim (resolves to itself on an i18n miss).
        let named = thread_label_display("Project chat");
        assert_eq!(named.key, "Project chat");
        assert_eq!(named.resolve(|_| None::<&str>), "Project chat");
        // Empty and whitespace-only labels both fall back to the canonical
        // `(no subject)` key (android's richest existing pattern; en.yaml L586).
        for blank in ["", "   ", "\t\n"] {
            assert_eq!(
                thread_label_display(blank).key,
                "conversations.detail.no_subject"
            );
        }
        // The key resolves to the localized placeholder via the client's pipeline.
        assert_eq!(
            thread_label_display("").resolve(|k| if k == "conversations.detail.no_subject" {
                Some("(no subject)")
            } else {
                None
            }),
            "(no subject)"
        );
    }

    #[test]
    fn share_link_labels_map_each_value_and_refuse_the_unknown() {
        for value in ["1d", "7d", "30d", "1y"] {
            assert_eq!(
                share_link_expiry_label(value).map(|t| t.key),
                Some(format!("share_link.expiry_{value}"))
            );
        }
        assert_eq!(share_link_expiry_label("never"), None);
        for state in ["active", "expired", "revoked"] {
            assert_eq!(
                share_link_state_label(state).map(|t| t.key),
                Some(format!("share_link.state_{state}"))
            );
        }
        assert_eq!(share_link_state_label("paused"), None);
    }

    #[test]
    fn media_sort_label_maps_each_key_and_falls_back_to_name() {
        assert_eq!(media_sort_label("name").key, "media.sort_name");
        assert_eq!(media_sort_label("size").key, "media.sort_size");
        assert_eq!(media_sort_label("date").key, "media.sort_date");
        // Unrecognized/empty → the enum's own default variant, not a blank or
        // raw-value render (the drift this lift retires — see linux/web).
        assert_eq!(media_sort_label("").key, "media.sort_name");
        assert_eq!(media_sort_label("bogus").key, "media.sort_name");
    }

    #[test]
    fn media_sort_direction_label_maps_the_flag() {
        assert_eq!(
            media_sort_direction_label(false).key,
            "media.sort_ascending"
        );
        assert_eq!(
            media_sort_direction_label(true).key,
            "media.sort_descending"
        );
    }

    #[test]
    fn os_maintenance_status_label_priority() {
        // Nothing pending (also the version-skew default) → up to date.
        assert_eq!(
            os_maintenance_status_label(0, false).key,
            "admin.nest_page.os_up_to_date"
        );
        // Updates but no reboot → updates pending.
        assert_eq!(
            os_maintenance_status_label(3, false).key,
            "admin.nest_page.os_updates_pending"
        );
        // A pending reboot is the headline, regardless of the update count.
        assert_eq!(
            os_maintenance_status_label(0, true).key,
            "admin.nest_page.os_restart_pending"
        );
        assert_eq!(
            os_maintenance_status_label(5, true).key,
            "admin.nest_page.os_restart_pending"
        );
    }

    #[test]
    fn mail_health_state_label_maps_every_state_and_unknown_to_attention() {
        for (state, key) in [
            ("off", "admin.mail_page.health_state_off"),
            ("bridge_down", "admin.mail_page.health_state_bridge_down"),
            ("blocklisted", "admin.mail_page.health_state_blocklisted"),
            (
                "queue_stalled",
                "admin.mail_page.health_state_queue_stalled",
            ),
            (
                "records_failing",
                "admin.mail_page.health_state_records_failing",
            ),
            ("warming_up", "admin.mail_page.health_state_warming_up"),
            ("delivering", "admin.mail_page.health_state_delivering"),
        ] {
            assert_eq!(mail_health_state_label(state).key, key);
        }
        // A newer nest's state never breaks an older app.
        assert_eq!(
            mail_health_state_label("on_fire").key,
            "admin.mail_page.health_state_unknown"
        );
        assert_eq!(
            mail_health_check_state_label("fail").key,
            "admin.mail_page.health_check_fail"
        );
        assert_eq!(
            mail_health_check_state_label("smouldering").key,
            "admin.mail_page.health_state_unknown"
        );
    }

    #[test]
    fn peer_display_label_prefers_nickname_then_display_name_then_account_label() {
        let id = "ab".repeat(32);
        // No overlay, no display name: byte-identical to account_display_label.
        for handle in [Some("alice"), Some(""), None] {
            let l = peer_display_label(None, None, handle, &id);
            assert_eq!(l.primary, account_display_label(handle, &id));
            assert_eq!(l.public, None);
        }
        // Blank nickname / display name count as absent.
        let l = peer_display_label(Some("  "), Some(" "), Some("alice"), &id);
        assert_eq!(l, peer_display_label(None, None, Some("alice"), &id));
        // Display name beats the handle.
        let l = peer_display_label(None, Some(" Alice A. "), Some("alice"), &id);
        assert_eq!(l.primary, "Alice A.");
        assert_eq!(l.public, None);
        // A nickname is primary, and the public name it replaced rides along.
        let l = peer_display_label(Some(" Mum "), Some("Alice A."), Some("alice"), &id);
        assert_eq!(l.primary, "Mum");
        assert_eq!(l.public.as_deref(), Some("Alice A."));
        let l = peer_display_label(Some("Mum"), None, None, &id);
        assert_eq!(l.public, Some(short_id(&id)));
    }

    #[test]
    fn contact_matches_filter_over_handle_domain_actor_id() {
        let h = Some("alice");
        let d = Some("example.com");
        let id = "ab12cd34ef";
        // Empty / whitespace-only query matches every row.
        assert!(contact_matches_filter("", h, d, id, None, &[]));
        assert!(contact_matches_filter("   ", h, d, id, None, &[]));
        // Case-insensitive substring over each searchable field.
        assert!(contact_matches_filter("ALI", h, d, id, None, &[])); // handle
        assert!(contact_matches_filter("example", h, d, id, None, &[])); // domain
        assert!(contact_matches_filter("CD34", h, d, id, None, &[])); // actor-id hex
        // The query is trimmed before matching.
        assert!(contact_matches_filter("  alice  ", h, d, id, None, &[]));
        // No field contains the query → no match.
        assert!(!contact_matches_filter("zzz", h, d, id, None, &[]));
        // Federated peer (handle/domain None) still matches on actor-id, and
        // doesn't spuriously match a handle/domain query.
        assert!(contact_matches_filter("ab12", None, None, id, None, &[]));
        assert!(!contact_matches_filter("alice", None, None, id, None, &[]));
    }

    #[test]
    fn contact_matches_filter_over_the_private_overlay() {
        let id = "ab12cd34ef";
        let labels = vec!["Family".to_string(), "Book club".to_string()];
        let nick = Some("Mum");
        // The nickname and each live label are searchable.
        assert!(contact_matches_filter("mum", None, None, id, nick, &labels));
        assert!(contact_matches_filter(
            "FAMILY", None, None, id, nick, &labels
        ));
        assert!(contact_matches_filter(
            "book", None, None, id, nick, &labels
        ));
        // …and nothing else is: no overlay, no match.
        assert!(!contact_matches_filter("family", None, None, id, None, &[]));
        assert!(!contact_matches_filter(
            "zzz", None, None, id, nick, &labels
        ));
    }

    #[test]
    fn contact_toggle_block_label_flips_on_state() {
        // The `profile-block-button` toggle: an already-`blocked` edge reads
        // "Unblock", else "Block" (the canonical en.yaml keys; profile.md
        // § Element table — "toggle on `contact_status`").
        assert_eq!(contact_toggle_block_label(true).key, "profile.unblock");
        assert_eq!(contact_toggle_block_label(false).key, "profile.block");
    }

    #[test]
    fn follow_toggle_label_flips_on_state() {
        // The `profile-follow-button` label: already following reads
        // "Following", else "Follow" (the canonical en.yaml keys; profile.md
        // § Element table — follow = subscribe to the free "followers" tier).
        assert_eq!(follow_toggle_label(true).key, "profile.following");
        assert_eq!(follow_toggle_label(false).key, "profile.follow");
    }

    #[test]
    fn contact_row_blocks_actor_matches_blocked_edge() {
        let target = "ab12cd34ef";
        // A `blocked` roster row for the target → the actor is blocked.
        assert!(contact_row_blocks_actor(target, "blocked", target));
        // Any other status for the target → not blocked.
        assert!(!contact_row_blocks_actor(target, "accepted", target));
        assert!(!contact_row_blocks_actor(target, "pending", target));
        assert!(!contact_row_blocks_actor(target, "confirmed", target));
        assert!(!contact_row_blocks_actor(target, "", target));
        // A `blocked` row for a *different* peer → not this actor's block.
        assert!(!contact_row_blocks_actor("other99", "blocked", target));
        // Actor-id match is case-insensitive: IDs are canonical lowercase hex, so
        // this never narrows a real match and tolerates a non-normalized id —
        // unifying windows' prior case-insensitive compare with the other four
        // apps' case-sensitive one (the lone divergence this lift resolves).
        assert!(contact_row_blocks_actor("AB12CD34EF", "blocked", target));
    }

    #[test]
    fn device_status_label_flips_on_state() {
        // The Devices page `device-status` label: an online device reads
        // "Online", else "Offline" (the canonical en.yaml keys; devices.md
        // § Element table — `device-status`).
        assert_eq!(device_status_label(true).key, "devices.online");
        assert_eq!(device_status_label(false).key, "devices.offline");
    }

    #[test]
    fn device_place_label_composes_the_wizard_place_labels() {
        // The devices-page `device-folder-role-badge` chip must not drift
        // into its own wording — it is composed from the SAME three keys the
        // create wizard's place checkboxes carry, one per set flag.
        let lookup = |k: &str| -> Option<String> {
            Some(
                match k {
                    "devices.wizard.place_originates" => "Up",
                    "devices.wizard.place_accepts" => "In",
                    "devices.wizard.place_applies_deletes" => "Del",
                    "devices.place_two" => "{first} · {second}",
                    "devices.place_three" => "{first} · {second} · {third}",
                    "devices.place_none" => "Idle",
                    _ => return None,
                }
                .to_string(),
            )
        };
        let text = |o, a, d| device_place_label(o, a, d).resolve_nested(lookup);
        assert_eq!(text(true, true, true), "Up · In · Del");
        assert_eq!(text(true, true, false), "Up · In");
        assert_eq!(text(true, false, false), "Up");
        assert_eq!(text(false, true, true), "In · Del");
        assert_eq!(text(false, false, true), "Del");
        assert_eq!(text(false, false, false), "Idle");
        // All eight points are distinct — no two places share a chip.
        let mut seen = std::collections::HashSet::new();
        for o in [false, true] {
            for a in [false, true] {
                for d in [false, true] {
                    assert!(seen.insert(text(o, a, d)), "({o}, {a}, {d}) collides");
                }
            }
        }
    }

    #[test]
    fn bunker_app_label_prefers_a_real_label_over_status() {
        // A labeled app renders its own label verbatim, regardless of status.
        let lt = bunker_app_label("My Signer", "active");
        assert_eq!(lt.key, "My Signer");
        let lt = bunker_app_label("My Signer", "pending");
        assert_eq!(lt.key, "My Signer");
    }

    #[test]
    fn bunker_app_label_falls_back_by_status_when_unlabeled() {
        assert_eq!(
            bunker_app_label("", "pending").key,
            "nostr.connected_apps.pending"
        );
        assert_eq!(
            bunker_app_label("", "active").key,
            "nostr.connected_apps.unnamed"
        );
    }

    #[test]
    fn bunker_last_used_label_flips_on_presence() {
        assert_eq!(
            bunker_last_used_label(None).key,
            "nostr.connected_apps.never_used"
        );
        let lt = bunker_last_used_label(Some("2026-07-23"));
        assert_eq!(lt.key, "nostr.connected_apps.last_used");
        assert_eq!(lt.args.get("time").map(String::as_str), Some("2026-07-23"));
    }

    #[test]
    fn claim_status_label_maps_the_three_states() {
        // The §5 manual-claims list `subscription-claim-status[i]` badge
        // (monetization.md § Pillar 3 — claims.list is the audit surface for
        // both manually- and webhook-minted codes).
        assert_eq!(
            claim_status_label(false, false).key,
            "subscriptions.claim_status_unredeemed"
        );
        assert_eq!(
            claim_status_label(true, false).key,
            "subscriptions.claim_status_redeemed"
        );
        assert_eq!(
            claim_status_label(false, true).key,
            "subscriptions.claim_status_voided"
        );
    }

    #[test]
    fn claim_status_label_prefers_redeemed_over_voided() {
        // Precedence is load-bearing, not incidental: a code that was redeemed
        // AND later voided reads "Redeemed" — the redemption is the fact the
        // author is auditing for. Pinned so a per-app re-hand-roll in the
        // other branch order is a test failure rather than a silent
        // cross-app disagreement.
        assert_eq!(
            claim_status_label(true, true).key,
            "subscriptions.claim_status_redeemed"
        );
    }

    #[test]
    fn provider_status_label_maps_the_three_states() {
        // The §4 provider row badge (monetization.md § Pillar 3 — Provider
        // status: evidence-based, no ping).
        assert_eq!(
            provider_status_label(None, None).key,
            "subscriptions.provider_status_configured"
        );
        assert_eq!(
            provider_status_label(Some(100), None).key,
            "subscriptions.provider_status_verified"
        );
        assert_eq!(
            provider_status_label(None, Some(100)).key,
            "subscriptions.provider_status_error"
        );
        // Most-recent evidence wins.
        assert_eq!(
            provider_status_label(Some(200), Some(100)).key,
            "subscriptions.provider_status_verified"
        );
        assert_eq!(
            provider_status_label(Some(100), Some(200)).key,
            "subscriptions.provider_status_error"
        );
        // A tie resolves conservative, toward error.
        assert_eq!(
            provider_status_label(Some(150), Some(150)).key,
            "subscriptions.provider_status_error"
        );
    }

    #[test]
    fn mail_serving_status_label_flips_on_state() {
        // The admin Users page `admin-users-mail-serving-status[i]` read-only
        // audit indicator: a user whose local IMAP/CalDAV serving is enabled
        // reads "Serving here", else "Not serving" (the canonical en.yaml keys
        // under `admin.users_page`; admin.md § Users element table).
        assert_eq!(
            mail_serving_status_label(true).key,
            "admin.users_page.serving_here"
        );
        assert_eq!(
            mail_serving_status_label(false).key,
            "admin.users_page.serving_disabled"
        );
    }

    #[test]
    fn dns_verdict_label_maps_each_verdict() {
        // The three real verdicts map to their canonical `admin.dns.status_*` keys
        // (the serde variant names of `fauna_client_dns::VerifyStatus`).
        assert_eq!(dns_verdict_label("Ok", &[]).key, "admin.dns.status_ok");
        assert_eq!(
            dns_verdict_label("Missing", &[]).key,
            "admin.dns.status_missing"
        );
        assert_eq!(
            dns_verdict_label("Mismatch", &[]).key,
            "admin.dns.status_mismatch"
        );
        // `Checking`, an absent verdict (""), and any wire/version-drift value all
        // read as the neutral "checking" — never a false green/red.
        assert_eq!(
            dns_verdict_label("Checking", &[]).key,
            "admin.dns.status_checking"
        );
        assert_eq!(dns_verdict_label("", &[]).key, "admin.dns.status_checking");
        assert_eq!(
            dns_verdict_label("Bogus", &[]).key,
            "admin.dns.status_checking"
        );
    }

    /// A red verdict must say **what public DNS actually served** — a bare
    /// "Mismatch" is a dead-end diagnostic (the 2026-07-29 live walkthrough: the
    /// admin's apex rows read `Mismatch` and the page could not answer "mismatch
    /// with what?").
    #[test]
    fn dns_verdict_label_mismatch_reports_what_was_observed() {
        let one = dns_verdict_label("Mismatch", &["1.2.3.4".to_string()]);
        assert_eq!(one.key, "admin.dns.status_mismatch_found");
        assert_eq!(one.args.get("found").map(String::as_str), Some("1.2.3.4"));

        // Several RRs at one name read as one list, in the order DNS served them.
        let many = dns_verdict_label("Mismatch", &["1.2.3.4".to_string(), "5.6.7.8".to_string()]);
        assert_eq!(
            many.args.get("found").map(String::as_str),
            Some("1.2.3.4, 5.6.7.8")
        );
    }

    /// Observed values are reported **only** where they mean something. `Missing`
    /// is `observed.is_empty()` by construction (`fauna_mail::dns::verify`), and a
    /// green row needs no forensics; a `Mismatch` that arrives with no observed
    /// values — only reachable from a nest too old to send them — degrades to the
    /// plain label rather than rendering "found ".
    #[test]
    fn dns_verdict_label_reports_observed_only_where_it_helps() {
        assert_eq!(
            dns_verdict_label("Mismatch", &[]).key,
            "admin.dns.status_mismatch"
        );
        let ok = dns_verdict_label("Ok", &["1.2.3.4".to_string()]);
        assert_eq!(ok.key, "admin.dns.status_ok");
        assert!(ok.args.is_empty(), "a green row needs no forensics");
        let missing = dns_verdict_label("Missing", &["stray".to_string()]);
        assert_eq!(missing.key, "admin.dns.status_missing");
        assert!(missing.args.is_empty());
    }

    #[test]
    fn cert_status_label_maps_each_state() {
        // The served-cert health state maps to its `admin.dns.cert.status_*` key
        // (the serde variant names of `fauna_protocol::tls::CertHealthState`).
        assert_eq!(
            cert_status_label("ValidTrusted").key,
            "admin.dns.cert.status_valid"
        );
        assert_eq!(
            cert_status_label("Expiring").key,
            "admin.dns.cert.status_expiring"
        );
        // `OnFloorRenewNeeded` (the fresh-nest default) and any drift value read as
        // "Renew needed" — matching web's `certStatusText` / linux's default arm.
        assert_eq!(
            cert_status_label("OnFloorRenewNeeded").key,
            "admin.dns.cert.status_on_floor"
        );
        assert_eq!(
            cert_status_label("Bogus").key,
            "admin.dns.cert.status_on_floor"
        );
    }

    #[test]
    fn connection_state_label_maps_each_state() {
        assert_eq!(connection_state_label("connected").key, "common.connected");
        assert_eq!(
            connection_state_label("connecting").key,
            "common.connecting"
        );
        assert_eq!(
            connection_state_label("disconnected").key,
            "common.disconnected"
        );
        // The fourth state: a persistently failing connect is named, not shown as
        // an indefinite "Connecting…" (transport.md § Connection-status indicator).
        assert_eq!(
            connection_state_label("unreachable").key,
            "common.cannot_connect"
        );
        // `unreachable` must not be confused with the transient states it replaces —
        // the whole point is that the user can tell them apart.
        assert_ne!(
            connection_state_label("unreachable").key,
            connection_state_label("connecting").key
        );
        assert_ne!(
            connection_state_label("unreachable").key,
            connection_state_label("disconnected").key
        );
        // Forward-compat: an older client meeting a newer state word degrades to the
        // honest weaker claim, never a blank indicator.
        assert_eq!(
            connection_state_label("some_future_state").key,
            "common.disconnected"
        );
    }

    #[test]
    fn cert_status_view_suppresses_the_expiry_of_a_self_signed_floor() {
        // The bug this contract exists to make unrepresentable: the floor cert
        // carries a genuine, far-future `not_after` (rcgen's default 4096-01-01 —
        // `bins/fauna-nest/src/acme.rs` mints it with no override), so a client
        // that tests `is_floor` and `not_after_unix > 0` independently renders
        // "Certificate: Renew needed (self-signed) — expires 4096-01-01": a
        // two-millennium expiry that reads reassuring on an untrusted cert.
        // Self-signed wins; the date is withheld.
        let v = cert_status_view("OnFloorRenewNeeded", true, 67_010_803_200);
        assert_eq!(v.state.key, "admin.dns.cert.status_on_floor");
        assert!(v.show_self_signed);
        assert_eq!(v.expires_at_unix, None);
    }

    #[test]
    fn cert_status_view_surfaces_the_expiry_of_a_trusted_cert() {
        let v = cert_status_view("ValidTrusted", false, 1_800_000_000);
        assert_eq!(v.state.key, "admin.dns.cert.status_valid");
        assert!(!v.show_self_signed);
        assert_eq!(v.expires_at_unix, Some(1_800_000_000));

        let v = cert_status_view("Expiring", false, 1_800_000_000);
        assert_eq!(v.state.key, "admin.dns.cert.status_expiring");
        assert_eq!(v.expires_at_unix, Some(1_800_000_000));
    }

    #[test]
    fn cert_status_view_withholds_an_absent_expiry() {
        // `not_after_unix` is 0 when the nest has no TLS resolver at all (plain
        // HTTP) — `tls_handlers.rs` sends the zero rather than an Option, so the
        // "> 0" guard is the contract, not a client's defensive habit.
        for not_after in [0, -1] {
            let v = cert_status_view("ValidTrusted", false, not_after);
            assert_eq!(v.expires_at_unix, None);
            assert!(!v.show_self_signed);
        }
    }

    #[test]
    fn cert_status_view_never_shows_both_sub_labels() {
        // The invariant every app relies on: the two sub-labels are mutually
        // exclusive by construction, across the whole input space.
        for state in ["ValidTrusted", "Expiring", "OnFloorRenewNeeded", "Bogus"] {
            for is_floor in [true, false] {
                for not_after in [-1, 0, 1_800_000_000] {
                    let v = cert_status_view(state, is_floor, not_after);
                    assert!(
                        !(v.show_self_signed && v.expires_at_unix.is_some()),
                        "both sub-labels for {state}/{is_floor}/{not_after}"
                    );
                    assert_eq!(v.state, cert_status_label(state));
                }
            }
        }
    }

    #[test]
    fn offer_status_precedence_active_pending_none() {
        // The `subscription-offers-section` per-tier badge. Precedence
        // Active > Pending > None (monetization.md § Pillar 1; profile.md
        // § Another's profile). `status_tier` is the viewer's confirmed held
        // tier (`status.get`); `pending` is a transient post-click flag
        // (`status.get` carries no pending discriminant).
        // Active: the viewer's confirmed tier matches — wins even over pending.
        assert_eq!(
            offer_status("gold", Some("gold"), true),
            OfferStatus::Active
        );
        assert_eq!(
            offer_status("gold", Some("gold"), false),
            OfferStatus::Active
        );
        // Pending: not the confirmed tier, but flagged transient-pending.
        assert_eq!(
            offer_status("gold", Some("silver"), true),
            OfferStatus::Pending
        );
        assert_eq!(offer_status("gold", None, true), OfferStatus::Pending);
        // None: neither confirmed nor pending.
        assert_eq!(
            offer_status("gold", Some("silver"), false),
            OfferStatus::None
        );
        assert_eq!(offer_status("gold", None, false), OfferStatus::None);
    }

    #[test]
    fn offer_status_label_keys() {
        assert_eq!(
            offer_status_label(OfferStatus::None).key,
            "subscriptions.offer_status_none"
        );
        assert_eq!(
            offer_status_label(OfferStatus::Pending).key,
            "subscriptions.offer_status_pending"
        );
        assert_eq!(
            offer_status_label(OfferStatus::Active).key,
            "subscriptions.offer_status_active"
        );
    }

    #[test]
    fn sync_display_state_label_maps_every_variant() {
        assert_eq!(
            sync_display_state_label(SyncDisplayState::Synced).key,
            "media.status_label.synced"
        );
        assert_eq!(
            sync_display_state_label(SyncDisplayState::LocalOnly).key,
            "media.status_label.local_only"
        );
        assert_eq!(
            sync_display_state_label(SyncDisplayState::RemoteOnly).key,
            "media.status_label.remote_only"
        );
        assert_eq!(
            sync_display_state_label(SyncDisplayState::Uploading).key,
            "media.status_label.uploading"
        );
        assert_eq!(
            sync_display_state_label(SyncDisplayState::Downloading).key,
            "media.status_label.downloading"
        );
        assert_eq!(
            sync_display_state_label(SyncDisplayState::Conflict).key,
            "media.status_label.conflict"
        );
    }
}

/// The lookup-generic text resolvers — the last step the two native Rust apps
/// used to hand-roll identically (`fauna-linux`'s `i18n`, `fauna-tui`'s
/// `format`). Every case here is a *composition* assertion: the shared decision
/// fns already have their own tests, so these pin only what moved — which slot
/// gets filled, with which of the two arms, and that an unfilled slot never
/// leaks its placeholder into painted text.
#[cfg(all(test, feature = "local-clock"))]
mod localized_resolve_tests {
    use super::*;

    const NOW: i64 = 1_700_000_000_000;

    /// A stand-in i18n table, transcribed from `i18n/strings/en.yaml` — every
    /// key these labels can select, so a leaked placeholder or an unresolved key
    /// is visible as itself rather than hidden behind a blanket `Some(key)`
    /// fallback. The arg names (`{count}`, `{value}`) are the real ones: a
    /// resolver that filled the wrong slot would pass against invented copy.
    fn lookup(key: &str) -> Option<&'static str> {
        match key {
            "time.just_now" => Some("just now"),
            "time.minutes_ago" => Some("{count}m ago"),
            "time.hours_ago" => Some("{count}h ago"),
            "time.days_ago" => Some("{count}d ago"),
            "backups.backup_destination_last_upload" => Some("Last synced: {when}"),
            "admin.mail_page.health_never" => Some("Never"),
            "admin.mail_page.health_state_delivering" => Some("Mail: delivering"),
            "admin.mail_page.health_state_unknown" => Some("Mail: needs attention"),
            "admin.mail_page.health_status_line" => {
                Some("{state} — last delivered: {delivered} · last received: {received}")
            }
            "backups.backup_destination_last_upload_never" => Some("Last synced: never"),
            "backups.backup_destination_last_audit" => Some("Last checked: {when}"),
            "backups.backup_destination_last_audit_never" => Some("Last checked: never"),
            "backups.backup_destination_usage" => Some("{held} of {cap} used"),
            "backups.backup_destination_usage_uncapped" => Some("{held} held, no limit set"),
            "size.bytes" => Some("{value} B"),
            "size.kb" => Some("{value} KB"),
            "size.mb" => Some("{value} MB"),
            "size.gb" => Some("{value} GB"),
            "size.tb" => Some("{value} TB"),
            _ => None,
        }
    }

    /// The bucket arm resolves through the caller's table; the `≥ 7 d` arm falls
    /// through to the shared local date instead. Both from one call — the whole
    /// reason the door exists.
    #[test]
    fn relative_time_text_renders_the_bucket_or_the_absolute_date() {
        assert_eq!(
            relative_time_text(NOW, NOW - 2 * MINUTE_MS, lookup),
            "2m ago"
        );
        assert_eq!(relative_time_text(NOW, NOW - 3 * HOUR_MS, lookup), "3h ago");

        let old_ms = NOW - 30 * DAY_MS;
        assert_eq!(
            relative_time_text(NOW, old_ms, lookup),
            format_unix_local_date_ms(old_ms),
        );
    }

    /// A display carrying neither arm renders empty rather than inventing a
    /// time — unreachable through [`relative_time_display`], and the honest
    /// answer if a future producer ever emits it.
    #[test]
    fn relative_time_display_text_renders_empty_when_neither_arm_is_set() {
        let empty = RelativeTimeDisplay {
            localized: None,
            absolute_epoch_ms: None,
        };
        assert_eq!(relative_time_display_text(&empty, lookup), "");
    }

    /// The never arm is complete as-is: no `{when}` in the label, so nothing to
    /// substitute and nothing left over.
    #[test]
    fn backup_upload_and_audit_never_arms_carry_no_slot() {
        for absent in [None, Some(0)] {
            assert_eq!(
                backup_last_upload_text(absent, NOW, lookup),
                "Last synced: never"
            );
            assert_eq!(
                backup_last_audit_text(absent, NOW, lookup),
                "Last checked: never"
            );
        }
    }

    /// The status line fills all three slots — the state's label and the two
    /// heartbeat facts, "Never" for an absent stamp — and an unknown state reads
    /// "needs attention" rather than leaking its token.
    #[test]
    fn mail_health_status_text_fills_state_and_both_heartbeats() {
        let two_min_ago_secs = (NOW - 2 * MINUTE_MS) / 1_000;
        assert_eq!(
            mail_health_status_text("delivering", Some(two_min_ago_secs), None, NOW, lookup),
            "Mail: delivering — last delivered: 2m ago · last received: Never"
        );
        let unknown = mail_health_status_text("on_fire", None, None, NOW, lookup);
        assert_eq!(
            unknown,
            "Mail: needs attention — last delivered: Never · last received: Never"
        );
        assert!(!unknown.contains('{'), "no placeholder survives: {unknown}");
        assert_eq!(mail_health_heartbeat_text(None, NOW, lookup), "Never");
        assert_eq!(
            mail_health_heartbeat_text(Some(two_min_ago_secs), NOW, lookup),
            "2m ago"
        );
    }

    /// A real timestamp fills `{when}` with the *resolved* inner text — the step
    /// that was duplicated. The two rows keep their distinct copy, which is why
    /// they are separate doors.
    #[test]
    fn backup_upload_and_audit_fill_when_with_the_resolved_inner_text() {
        let two_min_ago_secs = ((NOW - 2 * MINUTE_MS) / 1_000) as u64;

        let upload = backup_last_upload_text(Some(two_min_ago_secs), NOW, lookup);
        assert_eq!(upload, "Last synced: 2m ago");

        let audit = backup_last_audit_text(Some(two_min_ago_secs), NOW, lookup);
        assert_eq!(audit, "Last checked: 2m ago");

        // No placeholder survives into painted text.
        assert!(!upload.contains("{when}"));
        assert!(!audit.contains("{when}"));
    }

    /// Past the relative window, `{when}` takes the shared local date — the arm
    /// a terminal and a GTK label both render the same plain way.
    #[test]
    fn backup_upload_falls_back_to_the_local_date_past_the_window() {
        let old_ms = NOW - 30 * DAY_MS;
        let text = backup_last_upload_text(Some((old_ms / 1_000) as u64), NOW, lookup);
        assert_eq!(
            text,
            format!("Last synced: {}", format_unix_local_date_ms(old_ms)),
        );
    }

    /// Both inner byte texts resolve before substitution, and a label naming
    /// only `{held}` never grows a `{cap}`.
    #[test]
    fn backup_usage_fills_only_the_slots_its_label_names() {
        let capped = backup_usage_text(Some(2 * 1024 * 1024), Some(10 * 1024 * 1024), None, lookup);
        assert!(capped.contains("2 MB"), "held resolved: {capped}");
        assert!(capped.contains("10 MB"), "cap resolved: {capped}");
        assert!(!capped.contains("{held}") && !capped.contains("{cap}"));

        let uncapped = backup_usage_text(Some(2 * 1024 * 1024), None, None, lookup);
        assert!(uncapped.contains("2 MB"), "held resolved: {uncapped}");
        assert!(!uncapped.contains("{held}") && !uncapped.contains("{cap}"));
    }
}
