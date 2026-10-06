//! The admin Settings sub-page (`admin-settings`, nav label "Tiers") — the tier
//! *definitions* surface (`admin.md` § 3 Settings): what each tier *means* (its
//! `AdminTier` caps), edited in place. This is policy, distinct from *admission*
//! (assigning a tier to a user), which lives on `admin-users`. It also holds the
//! membership-designation section (monetization.md § Pillar 4): a link editor
//! over the admin's own subscription tiers, never a third tier list.
//!
//! Drives directly off the shared `AdminClient` (`fauna.admin.tiers.{list,update}`,
//! `fauna.admin.membership_tiers.{list,set,clear}`) + `SubscriptionsClient`
//! (`fauna.subscriptions.tiers.list`, the membership row set) — there is no tier
//! *machine* (`fauna-client-admin` is a thin one-method-per-kind client), so the
//! shell (`super`) owns the reads/writes and this file is paint only. Each
//! indexed `admin-settings-tier-item` row carries five editable raw-i64 cap
//! inputs (scoped under the row via `Element::within`, the `post-card`
//! positional-scope idiom) + a save button; save persists via `tiers.update` and
//! the shell re-reads so the row re-renders from persisted state. Each indexed
//! `admin-settings-membership-item` row mirrors the shape with three selects
//! (subscription tier + admitted/lapsed quota tier) + save/clear.
//!
//! Invite codes are NOT here (they moved to `admin-users`, admin.md § 3), so no
//! `admin-settings-invite-*` / `admin-settings-tier-select` is painted.

use fauna_i18n::strings::admin as t;
use fauna_ui_ids as ids;

use super::{Action, AdminField, AdminState, TierCap};
use crate::element::{Element, Field, Gesture, SelectTarget};
use crate::pages::Page;

const TIER_ITEM: &str = ids::ADMIN_SETTINGS_TIER_ITEM;
const TIER_ADD_SECTION: &str = ids::ADMIN_SETTINGS_TIER_ADD_SECTION;
const MEMBERSHIP_ITEM: &str = ids::ADMIN_SETTINGS_MEMBERSHIP_ITEM;

pub(super) fn settings_elements(state: &AdminState) -> Vec<Element> {
    let mut els = vec![
        Element::label(ids::ADMIN_SETTINGS_HEADING, t::settings_page::TITLE),
        Element::gesture_button(
            ids::ADMIN_NAV_BACK,
            t::EXIT,
            true,
            Gesture::Nav(Page::Conversations),
        )
        .nav_back(),
        // The tier-definitions section anchor (the e2e's `tiers_section_visible`
        // reads it). A `view` container has no terminal chrome, so it paints as a
        // heading label (the `admin-factory-reset-section` shape).
        Element::label(ids::ADMIN_SETTINGS_TIERS_SECTION, t::settings_page::TIERS),
    ];

    // One `admin-settings-tier-item` row per tier definition. The anchor is a FLAT
    // element (`count("admin-settings-tier-item")` = the tier count) showing the
    // tier name; the five cap inputs + save button nest under it positionally via
    // `.within(TIER_ITEM, i)`, so the driver's `scope="admin-settings-tier-item[i]"`
    // resolves them. The `name` identifies the row and is NOT editable.
    for (i, tier) in state
        .tiers
        .as_deref()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        els.push(Element::label(TIER_ITEM, tier.name.clone()));
        els.push(cap_input(state, i, TierCap::Inbox));
        els.push(cap_input(state, i, TierCap::Storage));
        els.push(cap_input(state, i, TierCap::Devices));
        els.push(cap_input(state, i, TierCap::BlobSize));
        els.push(cap_input(state, i, TierCap::Feeds));
        els.push(
            Element::gesture_button(
                ids::ADMIN_SETTINGS_TIER_SAVE_BUTTON,
                t::settings_page::SAVE_TIER,
                true,
                Gesture::Admin(Action::SaveTier { row: i }),
            )
            .within(TIER_ITEM, i),
        );
    }

    // The add-a-tier form (admin.md § 3 — *Defining a new tier*, IDs user-approved
    // 2026-10-04): a flat section anchor, then the name input, the same five cap
    // inputs a row carries (scoped under the section by the same positional
    // `.within` idiom, index 0 — the section is a singleton) and the add button.
    els.push(Element::label(
        TIER_ADD_SECTION,
        t::settings_page::ADD_TIER_SECTION,
    ));
    els.push(
        Element::input(
            ids::ADMIN_SETTINGS_TIER_ADD_NAME_INPUT,
            state.tier_add_name.clone(),
            Field::Admin(AdminField::TierAddName),
        )
        .labelled(t::settings_page::ADD_TIER_NAME)
        .within(TIER_ADD_SECTION, 0),
    );
    for cap in TierCap::ALL {
        let (id, label) = cap_id_and_label(cap);
        els.push(
            Element::input(
                id,
                state.tier_add_caps.get(cap).to_string(),
                Field::Admin(AdminField::TierAddCap(cap)),
            )
            .labelled(label)
            .within(TIER_ADD_SECTION, 0),
        );
    }
    els.push(
        Element::gesture_button(
            ids::ADMIN_SETTINGS_TIER_ADD_BUTTON,
            t::settings_page::ADD_TIER,
            true,
            Gesture::Admin(Action::AddTier),
        )
        .within(TIER_ADD_SECTION, 0),
    );

    // Membership designations (monetization.md § Pillar 4): the section anchor
    // always paints; one `admin-settings-membership-item` row per owned
    // subscription tier — the `admin-settings-tier-item` shape, three selects +
    // save/clear nested via `.within(MEMBERSHIP_ITEM, i)`. Designating creates
    // neither kind of tier, so an empty row set (no subscription tiers yet) is
    // the normal out-of-the-box state, not an error — zero rows paint.
    els.push(Element::label(
        ids::ADMIN_SETTINGS_MEMBERSHIP_SECTION,
        t::settings_page::MEMBERSHIP_SECTION,
    ));
    let quota_tier_names: Vec<String> = state
        .tiers
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|tier| tier.name.clone())
        .collect();
    let own_tier_names = state
        .own_membership_tier_names
        .as_deref()
        .unwrap_or_default();
    let designations = state.membership_tiers.as_deref().unwrap_or_default();
    for (i, draft) in state.membership_drafts.iter().enumerate() {
        // Clear is desensitized when this row carries no PERSISTED designation
        // yet (read off the row's own tier name, not the possibly-locally-edited
        // draft) — nothing to clear (linux's `clear.set_sensitive` shape).
        let has_existing = own_tier_names
            .get(i)
            .is_some_and(|name| designations.iter().any(|m| &m.tier_name == name));
        els.push(Element::label(MEMBERSHIP_ITEM, draft.tier_name.clone()));
        els.push(
            Element::select(
                ids::ADMIN_SETTINGS_MEMBERSHIP_TIER_SELECT,
                draft.tier_name.clone(),
                SelectTarget::MembershipTierName { row: i },
                own_tier_names.to_vec(),
            )
            .within(MEMBERSHIP_ITEM, i),
        );
        els.push(
            Element::select(
                ids::ADMIN_SETTINGS_MEMBERSHIP_ADMIN_TIER_SELECT,
                draft.admin_tier.clone(),
                SelectTarget::MembershipAdminTier { row: i },
                quota_tier_names.clone(),
            )
            .within(MEMBERSHIP_ITEM, i),
        );
        els.push(
            Element::select(
                ids::ADMIN_SETTINGS_MEMBERSHIP_LAPSE_TIER_SELECT,
                draft.lapse_tier.clone(),
                SelectTarget::MembershipLapseTier { row: i },
                quota_tier_names.clone(),
            )
            .within(MEMBERSHIP_ITEM, i),
        );
        els.push(
            Element::gesture_button(
                ids::ADMIN_SETTINGS_MEMBERSHIP_SAVE_BUTTON,
                t::settings_page::MEMBERSHIP_SAVE,
                true,
                Gesture::Admin(Action::SaveMembership { row: i }),
            )
            .within(MEMBERSHIP_ITEM, i),
        );
        els.push(
            Element::gesture_button(
                ids::ADMIN_SETTINGS_MEMBERSHIP_CLEAR_BUTTON,
                t::settings_page::MEMBERSHIP_CLEAR,
                has_existing,
                Gesture::Admin(Action::ClearMembership { row: i }),
            )
            .within(MEMBERSHIP_ITEM, i),
        );
    }

    els
}

/// The shared element id and visible label of one cap input — the same five
/// inputs sit under every tier row and under the add form.
fn cap_id_and_label(cap: TierCap) -> (&'static str, &'static str) {
    match cap {
        TierCap::Inbox => (
            ids::ADMIN_SETTINGS_TIER_CAP_INBOX,
            t::settings_page::CAP_INBOX_BYTES,
        ),
        TierCap::Storage => (
            ids::ADMIN_SETTINGS_TIER_CAP_STORAGE,
            t::settings_page::CAP_STORAGE_BYTES,
        ),
        TierCap::Devices => (
            ids::ADMIN_SETTINGS_TIER_CAP_DEVICES,
            t::settings_page::CAP_DEVICES,
        ),
        TierCap::BlobSize => (
            ids::ADMIN_SETTINGS_TIER_CAP_BLOB_SIZE,
            t::settings_page::CAP_BLOB_SIZE,
        ),
        TierCap::Feeds => (
            ids::ADMIN_SETTINGS_TIER_CAP_FEEDS,
            t::settings_page::CAP_FEEDS,
        ),
    }
}

/// One editable raw-i64 cap input, scoped under `admin-settings-tier-item[row]`.
/// The draft (what `get_text` reads back) is re-seeded from the persisted cap on
/// every `tiers.list`; a keystroke writes it via `AdminField::TierCap`.
fn cap_input(state: &AdminState, row: usize, cap: TierCap) -> Element {
    let (id, label) = cap_id_and_label(cap);
    let value = state
        .tier_cap_drafts
        .get(row)
        .map(|d| d.get(cap).to_string())
        .unwrap_or_default();
    Element::input(id, value, Field::Admin(AdminField::TierCap { row, cap }))
        .labelled(label)
        .within(TIER_ITEM, row)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use fauna_client_admin::AdminTier;
    use fauna_protocol::admin::AdminMembershipTier;

    use super::*;

    /// Every `admin.*` page-error string names the gesture AND carries the reason
    /// (`docs/goal/ui/README.md` § Copy comprehensibility, Q1). These were flat
    /// prefixes: tui painted them detail-less, and web rendered the bare exception
    /// *instead of* the string — so an admin saw either "what failed" or "why",
    /// never both. A key that loses its `{message}` fails here, on every app at once.
    #[test]
    fn every_admin_page_error_string_carries_its_detail() {
        use fauna_i18n::strings::admin;
        const DETAIL: &str = "the-underlying-reason";
        for (key, rendered) in [
            (
                "admin.dashboard.load_error",
                admin::dashboard::load_error(DETAIL),
            ),
            (
                "admin.settings_page.load_tiers_error",
                admin::settings_page::load_tiers_error(DETAIL),
            ),
            (
                "admin.settings_page.save_tier_error",
                admin::settings_page::save_tier_error(DETAIL),
            ),
            (
                "admin.settings_page.load_membership_tiers_error",
                admin::settings_page::load_membership_tiers_error(DETAIL),
            ),
            (
                "admin.settings_page.save_membership_tier_error",
                admin::settings_page::save_membership_tier_error(DETAIL),
            ),
            (
                "admin.settings_page.clear_membership_tier_error",
                admin::settings_page::clear_membership_tier_error(DETAIL),
            ),
            (
                "admin.nest_page.load_settings_error",
                admin::nest_page::load_settings_error(DETAIL),
            ),
            (
                "admin.nest_page.update_setting_error",
                admin::nest_page::update_setting_error(DETAIL),
            ),
            (
                "admin.nest_page.load_serving_port_error",
                admin::nest_page::load_serving_port_error(DETAIL),
            ),
            (
                "admin.nest_page.set_serving_port_error",
                admin::nest_page::set_serving_port_error(DETAIL),
            ),
            (
                "admin.nest_page.os_restart_now_error",
                admin::nest_page::os_restart_now_error(DETAIL),
            ),
        ] {
            assert!(
                rendered.ends_with(&format!(": {DETAIL}")),
                "{key} must carry the failure's own reason, not just a flat prefix: {rendered}"
            );
        }
    }

    /// The two LOCAL refusals have no wire detail to thread, so they answer Q2/Q4
    /// themselves: the rule that was broken, and the control that fixes it. A flat
    /// "Failed to save tier" would leave the admin guessing which box is wrong.
    #[test]
    fn the_local_save_refusals_name_the_rule_and_the_control() {
        use fauna_i18n::strings::admin::settings_page as t;
        assert!(
            t::SAVE_TIER_ERROR_INVALID_CAP.contains("whole number"),
            "the cap refusal names the rule: {}",
            t::SAVE_TIER_ERROR_INVALID_CAP
        );
        assert!(
            t::SAVE_MEMBERSHIP_TIER_ERROR_NO_TIER.contains("Admits at"),
            "the membership refusal names the control to fix, by its visible label: {}",
            t::SAVE_MEMBERSHIP_TIER_ERROR_NO_TIER
        );
    }

    fn tier(
        name: &str,
        inbox: i64,
        storage: i64,
        devices: i64,
        blob: i64,
        feeds: i64,
    ) -> AdminTier {
        AdminTier {
            name: name.to_string(),
            max_inbox_bytes: inbox,
            max_storage_bytes: storage,
            max_devices: devices,
            max_blob_size: blob,
            max_feeds: feeds,
            extra: BTreeMap::new(),
        }
    }

    /// `TiersLoaded` carrying no membership data — the shape every pure
    /// tier-cap test uses (membership is asserted by its own tests below).
    fn tiers_loaded(tiers: Vec<AdminTier>) -> super::super::Outcome {
        super::super::Outcome::TiersLoaded {
            tiers,
            own_membership_tier_names: Vec::new(),
            membership_tiers: Vec::new(),
        }
    }

    fn membership_tier(tier_name: &str, admin_tier: &str, lapse_tier: &str) -> AdminMembershipTier {
        AdminMembershipTier {
            tier_name: tier_name.to_string(),
            admin_tier: admin_tier.to_string(),
            lapse_tier: lapse_tier.to_string(),
            created_at: 0,
            extra: BTreeMap::new(),
        }
    }

    /// The page paints its heading, the tiers-section anchor, then one
    /// `admin-settings-tier-item` row per tier — each with five scoped cap inputs
    /// (re-seeded from the persisted caps) + a save button — followed by the
    /// membership-section anchor (zero rows here: no membership data loaded).
    #[test]
    fn settings_page_paints_section_and_indexed_tier_rows() {
        let mut app = crate::app::tests::test_app();
        app.admin.sub = super::super::AdminPage::Settings;
        super::super::apply_outcome(
            &mut app,
            tiers_loaded(vec![
                tier("free", 1000, 2000, 2, 500, 3),
                tier("personal", 4000, 8000, 5, 900, 10),
            ]),
        );

        let els = settings_elements(&app.admin);
        let tagged: Vec<&str> = els
            .iter()
            .map(|e| e.id.as_str())
            .filter(|id| !id.is_empty())
            .collect();
        assert_eq!(
            tagged,
            vec![
                "admin-settings-heading",
                "admin-nav-back",
                "admin-settings-tiers-section",
                // Row 0
                "admin-settings-tier-item",
                "admin-settings-tier-cap-inbox",
                "admin-settings-tier-cap-storage",
                "admin-settings-tier-cap-devices",
                "admin-settings-tier-cap-blob-size",
                "admin-settings-tier-cap-feeds",
                "admin-settings-tier-save-button",
                // Row 1
                "admin-settings-tier-item",
                "admin-settings-tier-cap-inbox",
                "admin-settings-tier-cap-storage",
                "admin-settings-tier-cap-devices",
                "admin-settings-tier-cap-blob-size",
                "admin-settings-tier-cap-feeds",
                "admin-settings-tier-save-button",
                // The add-a-tier form: anchor, name, the five caps, add button.
                "admin-settings-tier-add-section",
                "admin-settings-tier-add-name-input",
                "admin-settings-tier-cap-inbox",
                "admin-settings-tier-cap-storage",
                "admin-settings-tier-cap-devices",
                "admin-settings-tier-cap-blob-size",
                "admin-settings-tier-cap-feeds",
                "admin-settings-tier-add-button",
                // The membership section anchor always paints; zero rows here
                // (no membership data loaded by this test).
                "admin-settings-membership-section",
            ]
        );

        // Row 0's inbox draft was seeded from the persisted cap, and its input is
        // scoped under `admin-settings-tier-item[0]`.
        let inbox0 = els
            .iter()
            .find(|e| e.id == "admin-settings-tier-cap-inbox")
            .expect("inbox cap painted");
        assert_eq!(
            inbox0.text, "1000",
            "seeded from the persisted max_inbox_bytes"
        );
        assert_eq!(
            inbox0.path.len(),
            1,
            "the cap input is scoped under one tier-item"
        );

        // The two tier-item anchors show the (non-editable) tier names.
        let names: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "admin-settings-tier-item")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(names, vec!["free", "personal"]);
    }

    /// A `tiers.list` fold seeds every row's cap drafts from persisted state and
    /// clears a prior error (one fold for load + post-save re-read).
    #[test]
    fn tiers_loaded_seeds_cap_drafts_and_clears_error() {
        let mut app = crate::app::tests::test_app();
        app.errors.insert(Page::Admin, "stale".to_string());
        super::super::apply_outcome(
            &mut app,
            tiers_loaded(vec![tier("free", 1000, 2000, 2, 500, 3)]),
        );
        assert!(
            !app.errors.contains_key(&Page::Admin),
            "clean read clears the error"
        );
        let drafts = &app.admin.tier_cap_drafts[0];
        assert_eq!(drafts.inbox, "1000");
        assert_eq!(drafts.storage, "2000");
        assert_eq!(drafts.devices, "2");
        assert_eq!(drafts.blob_size, "500");
        assert_eq!(drafts.feeds, "3");
    }

    /// `SaveTier` parses the row's five drafts: all-valid → an `Op::UpdateTier`
    /// carrying the row's `name` + parsed caps + the preserved `extra`; any
    /// unparseable draft → the shared save-error on `error-message`, no op.
    #[test]
    fn save_tier_validates_and_carries_name_and_extra() {
        let mut app = crate::app::tests::test_app();
        let mut extra = BTreeMap::new();
        // A future additive field the client doesn't model — must survive the
        // update round-trip (additive-everywhere). `Value` is `fauna_cbor`'s Ipld.
        extra.insert("future_cap".to_string(), fauna_protocol::Value::Integer(7));
        super::super::apply_outcome(
            &mut app,
            tiers_loaded(vec![AdminTier {
                extra: extra.clone(),
                ..tier("free", 1000, 2000, 2, 500, 3)
            }]),
        );

        // Edit the inbox cap; the pure request-builder produces the update carrying
        // the (unedited) name, the parsed caps, and the preserved extra. (`apply_local`
        // itself needs a live client to build the Op, which a unit test has no way to
        // wire — the calendar test has the same shape — so the parse/build logic lives
        // in this pure helper precisely so it can be asserted here.)
        app.admin.tier_cap_drafts[0].inbox = "9999".to_string();
        let req = super::super::tier_update_req(&app.admin, 0).expect("valid caps → a request");
        assert_eq!(req.name, "free", "name identifies the row, unedited");
        assert_eq!(req.max_inbox_bytes, 9999, "the edited cap is carried");
        assert_eq!(req.max_feeds, 3);
        assert_eq!(req.extra, extra, "unknown fields are preserved (additive)");

        // An unparseable cap: the builder yields None, and `apply_local` surfaces the
        // save error on `error-message` and dispatches nothing.
        app.admin.tier_cap_drafts[0].storage = "not-a-number".to_string();
        assert!(super::super::tier_update_req(&app.admin, 0).is_none());
        let op = super::super::apply_local(&mut app, Action::SaveTier { row: 0 });
        assert!(op.is_none(), "an invalid cap dispatches no op");
        assert_eq!(
            app.errors.get(&Page::Admin).map(String::as_str),
            Some(t::settings_page::SAVE_TIER_ERROR_INVALID_CAP),
        );
    }

    /// A **negative** cap draft clamps to `0`, it does not ride the wire negative
    /// and it does not abandon the save.
    ///
    /// This is the one case the shared validator treats differently from every
    /// other bad input (`value-formatting.md` § Tier cap validation: "a negative
    /// is the one parseable case, clamped to `0` rather than dropped to `prev`"),
    /// and it is why this page must call `fauna_core::format::parse_cap` rather
    /// than its own `parse::<i64>()`: a bare parse succeeds on `-5`, so the row
    /// looked valid and sent a negative allowance to the nest. `0` is a
    /// meaningful cap here — an explicit "no allowance" — which is exactly why
    /// clamping is the right answer and refusing would be wrong.
    #[test]
    fn a_negative_cap_draft_clamps_to_zero_rather_than_riding_the_wire() {
        let mut app = crate::app::tests::test_app();
        super::super::apply_outcome(
            &mut app,
            tiers_loaded(vec![tier("free", 1000, 2000, 2, 500, 3)]),
        );

        app.admin.tier_cap_drafts[0].storage = "-5".to_string();
        let req = super::super::tier_update_req(&app.admin, 0)
            .expect("a negative is parseable — it clamps, it does not abandon the save");
        assert_eq!(req.max_storage_bytes, 0, "a negative cap must clamp to 0");
        assert_eq!(
            req.max_inbox_bytes, 1000,
            "the untouched caps are unchanged"
        );

        // The shared validator's other semantics ride along: surrounding
        // whitespace trims, and a leading `+` is a valid signed-int literal.
        app.admin.tier_cap_drafts[0].storage = "  7  ".to_string();
        app.admin.tier_cap_drafts[0].devices = "+4".to_string();
        let req = super::super::tier_update_req(&app.admin, 0).expect("valid caps → a request");
        assert_eq!(req.max_storage_bytes, 7);
        assert_eq!(req.max_devices, 4);
    }

    /// Fold seeds one `MembershipRowDraft` per owned subscription tier: an
    /// undesignated row defaults its admitted tier to empty and its lapse tier
    /// to the shared `DEFAULT_LAPSE_TIER`; a designated row shows its persisted
    /// values.
    #[test]
    fn membership_loaded_seeds_drafts_from_own_names_and_designations() {
        let mut app = crate::app::tests::test_app();
        super::super::apply_outcome(
            &mut app,
            super::super::Outcome::TiersLoaded {
                tiers: vec![],
                own_membership_tier_names: vec!["free".to_string(), "paid".to_string()],
                membership_tiers: vec![membership_tier("paid", "personal", "community")],
            },
        );
        let drafts = &app.admin.membership_drafts;
        assert_eq!(drafts.len(), 2);
        assert_eq!(drafts[0].tier_name, "free");
        assert_eq!(
            drafts[0].admin_tier, "",
            "undesignated row admits at nothing yet"
        );
        assert_eq!(
            drafts[0].lapse_tier,
            fauna_protocol::admin::DEFAULT_LAPSE_TIER,
            "undesignated row defaults its lapse tier to the shared default"
        );
        assert_eq!(drafts[1].tier_name, "paid");
        assert_eq!(drafts[1].admin_tier, "personal");
        assert_eq!(drafts[1].lapse_tier, "community");
    }

    /// The page paints the membership section anchor, then one
    /// `admin-settings-membership-item` row per owned subscription tier — each
    /// with three scoped selects + save/clear, clear desensitized on an
    /// undesignated row.
    #[test]
    fn settings_page_paints_membership_section_and_indexed_rows() {
        let mut app = crate::app::tests::test_app();
        app.admin.sub = super::super::AdminPage::Settings;
        super::super::apply_outcome(
            &mut app,
            super::super::Outcome::TiersLoaded {
                tiers: vec![tier("free", 1000, 2000, 2, 500, 3)],
                own_membership_tier_names: vec!["paid".to_string()],
                membership_tiers: vec![membership_tier("paid", "personal", "community")],
            },
        );

        let els = settings_elements(&app.admin);
        let membership_tagged: Vec<&str> = els
            .iter()
            .skip_while(|e| e.id != "admin-settings-membership-section")
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(
            membership_tagged,
            vec![
                "admin-settings-membership-section",
                "admin-settings-membership-item",
                "admin-settings-membership-tier-select",
                "admin-settings-membership-admin-tier-select",
                "admin-settings-membership-lapse-tier-select",
                "admin-settings-membership-save-button",
                "admin-settings-membership-clear-button",
            ]
        );

        let admin_select = els
            .iter()
            .find(|e| e.id == "admin-settings-membership-admin-tier-select")
            .expect("admin-tier select painted");
        assert_eq!(
            admin_select.text, "personal",
            "seeded from the persisted designation"
        );
        assert_eq!(
            admin_select.path.len(),
            1,
            "scoped under one membership-item"
        );

        let clear = els
            .iter()
            .find(|e| e.id == "admin-settings-membership-clear-button")
            .expect("clear button painted");
        assert!(clear.enabled, "a designated row's clear button is enabled");
    }

    /// `SaveMembership` parses the row's drafts into an upsert request; an empty
    /// admitted-tier draft (nothing to designate at) surfaces the shared
    /// membership save-error and dispatches nothing (the `SaveTier` shape).
    #[test]
    fn save_membership_validates_and_builds_upsert_request() {
        let mut app = crate::app::tests::test_app();
        super::super::apply_outcome(
            &mut app,
            super::super::Outcome::TiersLoaded {
                tiers: vec![],
                own_membership_tier_names: vec!["paid".to_string()],
                membership_tiers: vec![],
            },
        );

        // Undesignated: the empty admitted-tier draft yields no request.
        assert!(super::super::membership_save_req(&app.admin, 0).is_none());
        let op = super::super::apply_local(&mut app, Action::SaveMembership { row: 0 });
        assert!(op.is_none(), "an empty admitted tier dispatches no op");
        assert_eq!(
            app.errors.get(&Page::Admin).map(String::as_str),
            Some(t::settings_page::SAVE_MEMBERSHIP_TIER_ERROR_NO_TIER),
        );

        // Pick an admitted tier: a valid upsert request, carrying the row's own
        // tier name and an explicit (never omitted) lapse tier.
        app.admin.membership_drafts[0].admin_tier = "personal".to_string();
        let req = super::super::membership_save_req(&app.admin, 0)
            .expect("a picked admin tier → a request");
        assert_eq!(req.tier_name, "paid");
        assert_eq!(req.admin_tier, "personal");
        assert_eq!(
            req.lapse_tier.as_deref(),
            Some(fauna_protocol::admin::DEFAULT_LAPSE_TIER),
            "the lapse tier always rides as an explicit Some, never relying on omit-means-default"
        );
    }

    /// The add form's inputs are scoped under `admin-settings-tier-add-section`
    /// (index 0), never under a tier row — so a scoped driver query for a row's
    /// cap and for the add form's cap resolve to different elements.
    #[test]
    fn the_add_form_inputs_are_scoped_under_the_add_section() {
        let mut app = crate::app::tests::test_app();
        app.admin.sub = super::super::AdminPage::Settings;
        super::super::apply_outcome(&mut app, tiers_loaded(vec![tier("free", 1, 2, 3, 4, 5)]));
        app.admin.tier_add_name = "e2e-harness".to_string();

        let els = settings_elements(&app.admin);
        let name = els
            .iter()
            .find(|e| e.id == "admin-settings-tier-add-name-input")
            .expect("name input painted");
        assert_eq!(name.text, "e2e-harness", "the name draft is what paints");
        assert_eq!(
            name.path,
            vec![("admin-settings-tier-add-section".to_string(), 0)]
        );
        // The row's cap and the add form's cap share an id and differ only by scope.
        let inboxes: Vec<_> = els
            .iter()
            .filter(|e| e.id == "admin-settings-tier-cap-inbox")
            .collect();
        assert_eq!(
            inboxes.len(),
            2,
            "one under the row, one under the add form"
        );
        assert_eq!(inboxes[0].path[0].0, "admin-settings-tier-item");
        assert_eq!(inboxes[1].path[0].0, "admin-settings-tier-add-section");
    }

    fn add_form(name: &str, caps: [&str; 5]) -> super::super::AdminState {
        let mut state = super::super::AdminState {
            tier_add_name: name.to_string(),
            ..Default::default()
        };
        for (cap, value) in super::super::TierCap::ALL.into_iter().zip(caps) {
            state.tier_add_caps.set(cap, value.to_string());
        }
        state
    }

    /// The pure builder: a trimmed name and the five caps through the shared
    /// `parse_cap` validator (a negative clamps to 0, like the row editor).
    #[test]
    fn tier_create_req_builds_from_the_add_form() {
        let state = add_form(" e2e-harness ", ["111", "222", "64", "333", "1000"]);
        let req = super::super::tier_create_req(&state).expect("valid form");
        assert_eq!(req.name, "e2e-harness");
        assert_eq!(
            (
                req.max_inbox_bytes,
                req.max_storage_bytes,
                req.max_devices,
                req.max_blob_size,
                req.max_feeds
            ),
            (111, 222, 64, 333, 1000)
        );
        let clamped = add_form("t", ["-5", "0", "1", "1", "1"]);
        assert_eq!(
            super::super::tier_create_req(&clamped)
                .expect("a negative clamps, as on a row")
                .max_inbox_bytes,
            0
        );
    }

    #[test]
    fn tier_create_req_refuses_an_empty_name_and_a_bad_cap_locally() {
        use super::super::TierAddRefusal;
        let blank = add_form("   ", ["1", "1", "1", "1", "1"]);
        assert_eq!(
            super::super::tier_create_req(&blank).unwrap_err(),
            TierAddRefusal::EmptyName
        );
        for bad in ["", "1.5", "abc"] {
            let state = add_form("t", ["1", "1", bad, "1", "1"]);
            assert_eq!(
                super::super::tier_create_req(&state).unwrap_err(),
                TierAddRefusal::InvalidCap,
                "cap draft {bad:?}"
            );
        }
        // The refusals name the rule and the control, not a flat prefix.
        assert!(TierAddRefusal::EmptyName.to_string().contains("name"));
        assert!(
            TierAddRefusal::InvalidCap
                .to_string()
                .contains("whole number")
        );
    }

    /// A successful create clears the add form (and re-reads the list); a
    /// refusal never reaches this fold, so what was typed survives it.
    #[test]
    fn a_created_tier_clears_the_add_form() {
        let mut app = crate::app::tests::test_app();
        app.admin.tier_add_name = "e2e-harness".to_string();
        app.admin
            .tier_add_caps
            .set(super::super::TierCap::Feeds, "9".to_string());
        super::super::apply_outcome(
            &mut app,
            super::super::Outcome::TierAdded(Box::new(tiers_loaded(vec![tier(
                "e2e-harness",
                1,
                2,
                3,
                4,
                9,
            )]))),
        );
        assert!(app.admin.tier_add_name.is_empty());
        assert!(app.admin.tier_add_caps.feeds.is_empty());
        assert_eq!(app.admin.tiers.as_ref().map(Vec::len), Some(1));
    }
}
