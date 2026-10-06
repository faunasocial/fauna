//! The integration-depth selector's **rung catalog** — the one owner of the
//! four `atproto-depth-*` rungs' `(wire level, ui.yaml element id, title,
//! description, hosted)` rows (`docs/goal/ui/atproto.md` § Layout & flow item 1).
//!
//! That doc's § Where logic lives already says shared Rust owns "level logic,
//! transition-card content selection, gate interpretation, panel composition —
//! **all of it**", yet every one of the 7 apps hand-wrote the rung table
//! anyway: linux and tui as a `[(&str, &str); 4]` const plus their own 4-arm
//! title/description match, web as an inline array of four object literals,
//! android as a `listOf` plus a `rungTitles` map, windows and apple as their
//! own equivalents. This module closes that gap — the table was drift against
//! a ratified doc, not a design question.
//!
//! [`DepthLevelOption::hosted`] is carried rather than re-derived because the
//! same four apps each wrote `level.starts_with("hosted")` (or its Kotlin /
//! TypeScript / C# twin) to decide whether the hosted gate applies to a rung.
//! A string-prefix sniff over a closed vocabulary is a fact the catalog knows;
//! spelling it per app is how a fifth rung named `hosted_*`-but-ungated (or an
//! ungated rung that happens to start with the prefix) would silently mis-gate
//! on six surfaces at once.

use fauna_core::localized::LocalizedText;
use serde::{Deserialize, Serialize};

/// One rung of the integration-depth ladder as an app's selector renders it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DepthLevelOption {
    /// The level's wire spelling — what
    /// [`AtprotoSettingsSnapshot::level`](crate::snapshots::AtprotoSettingsSnapshot::level)
    /// reports and what `select_level` / `fauna.bridges.atproto.set_integration_level`
    /// take.
    pub level: String,
    /// The `ui.yaml` element id this rung carries.
    pub ui_id: String,
    /// The rung's name.
    pub title: LocalizedText,
    /// The rung's one-line meaning, in user voice (`atproto.md` § Goal's
    /// meaning column). Every app renders it — this is not an optional second
    /// line like the Nostr toggles' subtitle.
    pub description: LocalizedText,
    /// Whether entering this rung is subject to the hosted gate
    /// ([`AtprotoSettingsSnapshot::hosted_allowed`](crate::snapshots::AtprotoSettingsSnapshot::hosted_allowed)).
    /// A rung the user is *already at* stays selectable regardless, so a
    /// step-down is reachable even after the gate closes — that rule is the
    /// app's (`!gated || active`), but the `hosted` fact is this catalog's.
    pub hosted: bool,
}

impl DepthLevelOption {
    fn new(level: &str, ui_id: &str, title: &str, description: &str, hosted: bool) -> Self {
        DepthLevelOption {
            level: level.to_string(),
            ui_id: ui_id.to_string(),
            title: LocalizedText::key(title),
            description: LocalizedText::key(description),
            hosted,
        }
    }
}

/// The four integration-depth rungs, in the selector's top-to-bottom order —
/// which is also the ladder order, so a caller comparing two rungs' positions
/// is comparing their depth (`docs/goal/ui/atproto.md` § Layout & flow item 1).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn depth_level_options() -> Vec<DepthLevelOption> {
    vec![
        DepthLevelOption::new(
            "off",
            "atproto-depth-off",
            "atproto_settings.depth_off_title",
            "atproto_settings.depth_off_desc",
            false,
        ),
        DepthLevelOption::new(
            "linked",
            "atproto-depth-linked",
            "atproto_settings.depth_linked_title",
            "atproto_settings.depth_linked_desc",
            false,
        ),
        DepthLevelOption::new(
            "hosted_visible",
            "atproto-depth-hosted-visible",
            "atproto_settings.depth_hosted_visible_title",
            "atproto_settings.depth_hosted_visible_desc",
            true,
        ),
        DepthLevelOption::new(
            "hosted_full",
            "atproto-depth-hosted-full",
            "atproto_settings.depth_hosted_full_title",
            "atproto_settings.depth_hosted_full_desc",
            true,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshots::AtprotoSettingsSnapshot;

    /// The catalog is the four ratified rungs in ladder order, each carrying
    /// the wire level and element id `docs/goal/ui/atproto.md` § Layout & flow
    /// item 1 and `tests/e2e-unified/ui.yaml` declare.
    #[test]
    fn the_catalog_is_the_four_rungs_in_ladder_order() {
        let rows: Vec<(String, String)> = depth_level_options()
            .into_iter()
            .map(|o| (o.level, o.ui_id))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("off".to_string(), "atproto-depth-off".to_string()),
                ("linked".to_string(), "atproto-depth-linked".to_string()),
                (
                    "hosted_visible".to_string(),
                    "atproto-depth-hosted-visible".to_string()
                ),
                (
                    "hosted_full".to_string(),
                    "atproto-depth-hosted-full".to_string()
                ),
            ]
        );
    }

    /// Exactly the two hosted rungs are gate-subject. Pinned by name rather
    /// than by the `starts_with("hosted")` sniff the apps used to run, so this
    /// test would still catch a future rung whose name and gating disagree —
    /// which is the whole reason the field is carried instead of re-derived.
    #[test]
    fn only_the_two_hosted_rungs_are_gate_subject() {
        let gated: Vec<String> = depth_level_options()
            .into_iter()
            .filter(|o| o.hosted)
            .map(|o| o.level)
            .collect();
        assert_eq!(gated, vec!["hosted_visible", "hosted_full"]);
    }

    /// Every title and description is a dotted i18n key under
    /// `atproto_settings.`, never literal display text.
    #[test]
    fn rung_text_is_atproto_settings_i18n_keys() {
        for o in depth_level_options() {
            for (what, text) in [("title", &o.title), ("description", &o.description)] {
                assert!(
                    text.key.starts_with("atproto_settings.depth_"),
                    "{} {} must be an i18n key, got {:?}",
                    o.level,
                    what,
                    text.key
                );
                assert!(text.args.is_empty(), "{} {} takes no args", o.level, what);
            }
        }
    }

    /// The snapshot's own default level is a rung of this catalog — the page
    /// renders off `AtprotoSettingsSnapshot::default()` before the first
    /// hydrate, so a default outside the catalog would paint a selector with
    /// nothing marked active.
    #[test]
    fn the_default_snapshot_level_is_one_of_the_rungs() {
        let default_level = AtprotoSettingsSnapshot::default().level;
        assert!(
            depth_level_options()
                .iter()
                .any(|o| o.level == default_level),
            "default level {default_level:?} is not a rung of the catalog"
        );
    }
}
