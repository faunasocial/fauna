//! The Settings Privacy sub-page (split out of `settings/mod.rs`).

use fauna_client_email::email::filter_is_editable;
use fauna_i18n::strings::status::spam;
use fauna_i18n::strings::{common, settings as t};
/// The four inbox-acceptance modes: wire token + label, in the canonical
/// button order. **Both halves shared** — this file and linux's
/// `settings/privacy.rs` each used to carry the table, each under a comment
/// promising it matched the other, which no build checked (priority #2/#3).
use fauna_protocol::contacts::INBOX_MODES;
use fauna_ui_ids as ids;

use super::{Action, FILTER_ACTION_TYPES, FILTER_RULE_TYPES, SettingsField, SettingsState};
use crate::element::{Element, Field, Gesture, SelectTarget};

/// The localized threshold-band label ("Aggressive"/"Moderate"/"Permissive")
/// for the current `spam-threshold`/`phishing-threshold` buffer, via the
/// shared `fauna_protocol::spam` band mapping (`settings.md` § Spam threshold
/// slider labels) — the single source of truth every app renders from, both the
/// buckets and the label map. Only the buffer parse below is tui's own.
/// An unparseable buffer reads as the nest's own default (0.5, Moderate)
/// rather than panicking.
pub(super) fn spam_band_label(threshold_input: &str) -> &'static str {
    let probability: f64 = threshold_input.trim().parse().unwrap_or(0.5);
    fauna_client_spam::spam::spam_band_label(probability)
}

/// The Privacy sub-page (`settings.md` § Layout & flow items 5–7): inbox mode,
/// spam-classifier preferences, email filter rules, and `settings-nav-back` —
/// mirrors linux `settings/privacy.rs`. By the time this is ever built,
/// `state.privacy` already carries live data (`route_subpage`'s
/// `Op::FetchPrivacy` is awaited on the nav edge before this page can render at
/// all), so — unlike `quota-section` — nothing here is gated on a pending
/// fetch. The page's `error-message` is registered globally by
/// [`crate::ui::register_frame`] (the `tui-settings`/`logs`/`account`
/// precedent), so it is not painted here.
/// `filter_marks` is the cached open set from `App::filter_marks` — the ids of
/// rules a succession carried across that the owner has not yet kept or removed
/// (`succession-aftermath.md` § Adjudicating what the aftermath carries across).
/// Passed in rather than read from `state.privacy` because it is account-plane
/// state shared with the Account page's review line, not filter-page state.
pub(super) fn privacy_elements(state: &SettingsState, filter_marks: &[i64]) -> Vec<Element> {
    let p = &state.privacy;
    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::privacy_page::TITLE),
        Element::chrome(t::privacy_page::INBOX_MODE_DESCRIPTION),
    ];
    // ── Inbox mode (four mutually-exclusive buttons; ui.yaml component
    //    `inbox-mode-selector` names only the children, not a container id).
    //    One-of-N, so radio paint `(*)` — a checkbox stack read as four
    //    independent toggles (the control-kind-legibility rule). ──
    for (mode, label) in INBOX_MODES {
        let selected = p.inbox_mode.as_deref() == Some(mode);
        els.push(
            Element::radio_gesture(
                format!("inbox-mode-{mode}"),
                label,
                selected,
                Gesture::Settings(Action::SetInboxMode(mode.to_string())),
            )
            .attr("state", if selected { "on" } else { "off" }),
        );
    }
    // ── Spam moderation (ui.yaml component `spam-moderation-controls`) ──
    els.push(Element::label(
        ids::SPAM_PREFERENCES,
        t::privacy_page::SPAM_PROTECTION,
    ));
    els.push(
        Element::input(
            ids::SPAM_THRESHOLD,
            p.spam_threshold_input.clone(),
            Field::Settings(SettingsField::SpamThreshold),
        )
        .labelled(spam::SPAM_THRESHOLD),
    );
    // The band label (Aggressive/Moderate/Permissive) — untagged chrome, like
    // linux's `dim-label` suffix (no ui.yaml id of its own).
    els.push(Element::chrome(spam_band_label(&p.spam_threshold_input)));
    els.push(
        Element::input(
            ids::PHISHING_THRESHOLD,
            p.phishing_threshold_input.clone(),
            Field::Settings(SettingsField::PhishingThreshold),
        )
        .labelled(spam::PHISHING_THRESHOLD),
    );
    els.push(Element::gesture_button(
        ids::SAVE_SPAM_PREFS,
        spam::SAVE,
        true,
        Gesture::Settings(Action::SaveSpamPrefs),
    ));
    // ── Email filters (ui.yaml component `email-filter-panel`) ──
    els.push(Element::chrome(t::privacy_page::EMAIL_FILTERS));
    if p.filters.is_empty() {
        els.push(Element::chrome(t::privacy_page::NO_FILTERS_CONFIGURED));
    }
    // Flat indexed — one `filter-item`/`filter-name`/`filter-action`/
    // `filter-delete` per row in registration order, matching the
    // conversations-bubble-children convention (NOT a `within()` scope) —
    // EXCEPT `filter-edit`, which the shared e2e action (`filter_edit_visible`)
    // queries by real ancestor scope (`scope="filter-item[i]"`, the post-card
    // convention), so it alone carries `.within(ids::FILTER_ITEM, i)`.
    for (i, filter) in p.filters.iter().enumerate() {
        els.push(Element::label(ids::FILTER_ITEM, String::new()));
        els.push(Element::label(ids::FILTER_NAME, filter.name.clone()));
        els.push(Element::label(
            ids::FILTER_ACTION,
            crate::format::filter_action_label(&filter.action),
        ));
        // Gated: a filter only a raw API call could have produced (multi-rule,
        // or a richer rule/action no dialog collects) never opens a form that
        // would silently narrow it on save.
        if filter_is_editable(filter) {
            els.push(
                Element::gesture_button(
                    ids::FILTER_EDIT,
                    common::EDIT,
                    true,
                    Gesture::Settings(Action::OpenEditFilterForm(filter.id)),
                )
                .within(ids::FILTER_ITEM, i),
            );
        }
        // ── The post-succession review mark, and its Keep half ──
        //
        // Renders ONLY on a rule the aftermath carried across and the owner has
        // not adjudicated (`succession-aftermath.md` § Adjudicating what the
        // aftermath carries across, the fourth plane). This surface is the
        // whole point of the ruling: the per-action disposition justified
        // moving `Discard`/`FileInto` rules intact with *"the successor's own
        // filter list is its remedy"*, and until now nothing routed anyone here
        // or said which rules it meant.
        //
        // ⚠ There is deliberately **no** `filter-review-remove-button`. Remove
        // is `filter-delete`, one line down — the no-second-removal-mechanism
        // rule. A second removal control would be a second way to delete a
        // rule, and this plane's `Removed` is *recorded*, never enforced, so
        // recording it without deleting would leave an armed rule under a list
        // that now reads clean.
        if filter_marks.contains(&filter.id) {
            els.push(
                Element::label(
                    ids::FILTER_UNATTESTED_MARK,
                    t::privacy_page::FILTER_INHERITED,
                )
                .within(ids::FILTER_ITEM, i),
            );
            els.push(
                Element::gesture_button(
                    ids::FILTER_REVIEW_KEEP_BUTTON,
                    t::privacy_page::FILTER_KEEP,
                    true,
                    Gesture::Settings(Action::KeepFilter(filter.id)),
                )
                .within(ids::FILTER_ITEM, i),
            );
        }
        els.push(Element::gesture_button(
            ids::FILTER_DELETE,
            common::DELETE,
            true,
            Gesture::Settings(Action::DeleteFilter(filter.id)),
        ));
    }
    els.push(Element::gesture_button(
        ids::ADD_FILTER_BTN,
        t::privacy_page::ADD_FILTER,
        true,
        Gesture::Settings(Action::ToggleAddFilterForm),
    ));
    if p.show_add_form {
        els.push(
            Element::input(
                ids::FILTER_NAME_INPUT,
                p.filter_name_input.clone(),
                Field::Settings(SettingsField::FilterNameInput),
            )
            .labelled(t::FILTER_NAME),
        );
        els.push(
            Element::select(
                ids::FILTER_RULE_TYPE,
                p.filter_rule_type.clone(),
                SelectTarget::EmailFilterRuleType,
                FILTER_RULE_TYPES.iter().map(|s| s.to_string()).collect(),
            )
            .labelled(t::RULE_TYPE),
        );
        els.push(
            Element::input(
                ids::FILTER_RULE_VALUE,
                p.filter_rule_value.clone(),
                Field::Settings(SettingsField::FilterRuleValue),
            )
            .labelled(t::RULE_VALUE),
        );
        els.push(
            Element::select(
                ids::FILTER_ACTION_SELECT,
                p.filter_action.kind.clone(),
                SelectTarget::EmailFilterAction,
                FILTER_ACTION_TYPES.iter().map(|s| s.to_string()).collect(),
            )
            .labelled(t::privacy_page::ACTION),
        );
        // The Forward action's own inputs (`mail-forwarding.md` § Per-rule
        // "forward to"): the destination, and the copy mode as a "keep a local
        // copy" checkbox, checked by default — present only while Forward is
        // the selected action.
        if p.filter_action.kind == "Forward" {
            els.push(
                Element::input(
                    ids::FILTER_FORWARD_ADDRESS,
                    p.filter_action.forward_address.clone(),
                    Field::Settings(SettingsField::FilterForwardAddress),
                )
                .labelled(t::privacy_page::FORWARD_ADDRESS),
            );
            els.push(Element::checkbox_gesture(
                ids::FILTER_KEEP_LOCAL_COPY,
                t::privacy_page::KEEP_LOCAL_COPY,
                p.filter_action.keep_local_copy,
                Gesture::Settings(Action::ToggleFilterKeepLocalCopy),
            ));
        }
        // `create-filter`/`save-filter` are mutually exclusive (create-dialog's
        // counterpart while `editing_filter_id` is `Some`) — distinct ui.yaml
        // IDs so create vs. edit stay semantically separate on every app
        // (2026-07-16 user-approved shape).
        if p.editing_filter_id.is_some() {
            els.push(Element::gesture_button(
                ids::SAVE_FILTER,
                common::SAVE,
                true,
                Gesture::Settings(Action::SaveFilter),
            ));
        } else {
            els.push(Element::gesture_button(
                ids::CREATE_FILTER,
                common::CREATE,
                true,
                Gesture::Settings(Action::CreateFilter),
            ));
        }
    }
    els.push(
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
    );
    els
}
