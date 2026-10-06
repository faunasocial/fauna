//! The Nostr page's **content-toggle catalog** — the one owner of the five
//! content-publishing flags' `(ui.yaml element id, wire settings key, default,
//! label)` rows (`docs/goal/ui/nostr.md` § Where logic lives, § Layout & flow
//! item 2).
//!
//! Before this, every one of the 7 apps hand-wrote the same table: tui as a
//! `[(&str, &str, bool); 5]` const plus a 5-arm label match, linux as five
//! individually-named `SwitchRow` builders with the keys re-spelled twice more
//! in its read/write paths, web as the five keys spelled three times in one
//! component, android/windows/apple as one call or tuple per row. Six of the
//! seven escaped the cross-language duplicate-table scanner entirely, because
//! they build the id and key strings from variables or generated `uiIds`
//! constants rather than adjacent literals — so the duplication was invisible
//! to the tool that looks for it (priority #1/#2/#4).
//!
//! The shape is the one this project already uses for a picker vocabulary a
//! non-Rust app cannot compute: hand the **whole table** across the boundary,
//! the app owns only the widget. See `fauna_core::format::unknown_sender_options`
//! (the original) and `fauna_client_mail_settings::local_domains::role_address_options`
//! (the most recent). The native door is
//! `fauna_ffi::bridges::nostr_content_toggle_options`; the web door is
//! `nostrContentToggleOptions`.

use fauna_core::localized::LocalizedText;
use serde::{Deserialize, Serialize};

/// One bridge-settings boolean toggle as an app's settings screen needs it:
/// what to render it as, what to read/write it under, and what it means when
/// nobody has ever touched it.
///
/// Not Nostr-specific by type — every bridge whose settings are a flat set of
/// booleans renders these same columns — but [`nostr_content_toggle_options`]
/// is the only catalog today, so the *rows* live beside it rather than in a
/// registry no second caller has asked for yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeToggleOption {
    /// The `fauna.bridges.set_settings` key, and the key
    /// [`crate::settings::bool_setting`] reads the current value under.
    pub key: String,
    /// The `ui.yaml` element id the toggle carries — the automation contract's
    /// id, so a driver's `set_toggle(id)` and the app's own render agree by
    /// construction rather than by two people typing the same string.
    pub ui_id: String,
    /// What the toggle shows when the bridge reports no value for [`Self::key`].
    /// Mirrors the **nest's** own default, so a never-configured account paints
    /// what the nest would actually do rather than a client-side guess.
    ///
    /// Named `default_on`, not `default`, deliberately: `default` is a keyword
    /// in both C# and Swift, so a field spelled that way reaches two of the
    /// seven apps needing whatever escaping their bindgen happens to apply
    /// (`@default`, backticks) — a per-app spelling difference in a field
    /// whose whole point is that every app names it the same way. The wire
    /// key is unaffected; this is the *record field*'s name only.
    pub default_on: bool,
    /// The row's title. A [`LocalizedText`] key each app resolves through its
    /// own i18n runtime (linux `fauna_i18n`, android `resolveLocalized`, web
    /// `L()`, windows `Strings.Resolve`, apple `Bundle`).
    pub label: LocalizedText,
    /// The row's explanatory second line, where the vocabulary has one. `None`
    /// for the two rows whose titles are self-explanatory — an app with no
    /// subtitle affordance simply ignores the field.
    pub subtitle: Option<LocalizedText>,
}

impl BridgeToggleOption {
    fn new(key: &str, ui_id: &str, default_on: bool, label: &str, subtitle: Option<&str>) -> Self {
        BridgeToggleOption {
            key: key.to_string(),
            ui_id: ui_id.to_string(),
            default_on,
            label: LocalizedText::key(label),
            subtitle: subtitle.map(LocalizedText::key),
        }
    }
}

/// The five Nostr content-publishing toggles, in the ratified render order
/// (`docs/goal/ui/nostr.md` § Layout & flow item 2).
///
/// The defaults mirror the nest's own: `publish_replies` and `inbound_to_feed`
/// are ON, the other three OFF. The labels are the **title + subtitle** pairs
/// linux and tui already render — the richest of the two shapes in the tree
/// (web/android/apple/windows render a single longer line), so adopting the
/// catalog moves every app onto the pair rather than onto the shorter half
/// (priority #4).
pub fn nostr_content_toggle_options() -> Vec<BridgeToggleOption> {
    vec![
        BridgeToggleOption::new(
            "expose_content",
            "nostr-expose-content",
            false,
            "nostr.settings.expose_title",
            Some("nostr.settings.expose_subtitle"),
        ),
        BridgeToggleOption::new(
            "auto_publish",
            "nostr-auto-publish",
            false,
            "nostr.settings.auto_publish",
            Some("nostr.settings.auto_publish_subtitle"),
        ),
        BridgeToggleOption::new(
            "publish_replies",
            "nostr-publish-replies",
            true,
            "nostr.settings.publish_replies",
            None,
        ),
        BridgeToggleOption::new(
            "publish_reactions",
            "nostr-publish-reactions",
            false,
            "nostr.settings.publish_reactions",
            None,
        ),
        BridgeToggleOption::new(
            "inbound_to_feed",
            "nostr-inbound-to-feed",
            true,
            "nostr.settings.inbound_title",
            Some("nostr.settings.inbound_subtitle"),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalog is the five ratified rows in the ratified order, each
    /// carrying the exact wire key and element id `docs/goal/ui/nostr.md`
    /// § Layout & flow item 2 and `tests/e2e-unified/ui.yaml` declare. This is
    /// the assertion the seven hand-copies each made silently; making it once
    /// is the point of the lift.
    #[test]
    fn the_catalog_is_the_five_ratified_rows_in_order() {
        let opts = nostr_content_toggle_options();
        let rows: Vec<(&str, &str, bool)> = opts
            .iter()
            .map(|o| (o.key.as_str(), o.ui_id.as_str(), o.default_on))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("expose_content", "nostr-expose-content", false),
                ("auto_publish", "nostr-auto-publish", false),
                ("publish_replies", "nostr-publish-replies", true),
                ("publish_reactions", "nostr-publish-reactions", false),
                ("inbound_to_feed", "nostr-inbound-to-feed", true),
            ]
        );
    }

    /// Every element id is the wire key with `_`→`-` under a `nostr-` prefix.
    /// Not a decorative property: it is what lets a reader of either column
    /// trust the other, and a future sixth flag that breaks it is a typo, not
    /// a naming choice.
    #[test]
    fn each_element_id_is_the_nostr_prefixed_kebab_of_its_wire_key() {
        for o in nostr_content_toggle_options() {
            assert_eq!(
                o.ui_id,
                format!("nostr-{}", o.key.replace('_', "-")),
                "element id and wire key must not drift for {}",
                o.key
            );
        }
    }

    /// Every label (and every subtitle that exists) is a dotted i18n key under
    /// `nostr.settings.`, never literal display text — an app resolves these
    /// through its own runtime, so a raw string here would ship untranslated
    /// on all 7 apps at once.
    #[test]
    fn labels_are_nostr_settings_i18n_keys_not_literal_text() {
        for o in nostr_content_toggle_options() {
            assert!(
                o.label.key.starts_with("nostr.settings."),
                "{} label must be an i18n key, got {:?}",
                o.key,
                o.label.key
            );
            assert!(o.label.args.is_empty(), "{} label takes no args", o.key);
            if let Some(sub) = &o.subtitle {
                assert!(
                    sub.key.starts_with("nostr.settings."),
                    "{} subtitle must be an i18n key, got {:?}",
                    o.key,
                    sub.key
                );
            }
        }
    }

    /// Exactly the two flags the nest turns on by default read as ON. Pinned
    /// as a set rather than positionally so a reordering of the catalog cannot
    /// quietly move a default onto the wrong row.
    #[test]
    fn only_publish_replies_and_inbound_to_feed_default_on() {
        let on: Vec<String> = nostr_content_toggle_options()
            .into_iter()
            .filter(|o| o.default_on)
            .map(|o| o.key)
            .collect();
        assert_eq!(on, vec!["publish_replies", "inbound_to_feed"]);
    }
}
