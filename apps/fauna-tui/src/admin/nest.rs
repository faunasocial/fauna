//! The admin Nest sub-page (`admin-nest`) — nest-wide settings that aren't a
//! feature page, plus the danger zone (`admin.md` § N Nest).
//!
//! Nine concerns, each a dumb render of a shared read/write (priority #2 — no
//! nest or shared-Rust work here; the shell (`super`) owns the machine, ops and
//! folds, this file is paint only):
//!
//! - **Pairing** (`admin-service-pairing-toggle`/`-status`) — the admin
//!   user-pairing policy, over `fauna.admin.services.{list,update}` name `pairing`.
//! - **Serving port** (`admin-nest-serving-port-input`/`-save-button`) — the
//!   client-facing API port (`fauna.admin.set_serving_port`; read via
//!   `fauna.setup.status`), inert behind the SNI router.
//! - **NAT mode** (`admin-nest-nat-mode-*`) — the shared `AdminNatModeMachine`
//!   (`fauna.setup.nat_mode`); the wizard's `nat_mode_choice` shape, post-onboarding.
//! - **Host-OS maintenance** (`nest-os-maintenance-status` / `-updates-count` /
//!   `-restart-now-button`) — the read-only patch/reboot indicator + "restart now"
//!   (`fauna.admin.request_host_restart`), from `fauna.setup.status` os_* fields.
//! - **Declared region** (`admin-nest-region-*`) — the deployment's legal situs
//!   (`fauna.admin.region.{get,set}`), every rendering decision made once in the
//!   shared `fauna_client_admin::admin_region_view`. Declared, never detected:
//!   no detect affordance may be added here (`region-blocking.md` § Region
//!   determination, user-ratified).
//! - **Deployment identity** (`admin-nest-seed-rotate-*`) — the deployment-seed
//!   rotation ceremony (`box-recovery.md` § Deployment-seed rotation), driven by
//!   the shared plane drive
//!   `fauna_client_account_runtime::deployment_seeds::rotate_deployment_seed`. Inline-confirm like the
//!   reset below, except the confirm carries the **roster listing** the doc's
//!   ordering rule requires (the set that inherits the successor seed) and stays
//!   disabled until that listing can be shown.
//! - **Outside-app sign-in keys** (`admin-nest-oauth-*`) — the nest-held OAuth
//!   issuer key set and its refresh-token secret (`authorization-server.md` §
//!   The issuer → *Two rotation arms*): the served keys with each retired one's
//!   countdown, the ordinary rotation (no confirm — nothing breaks), and the two
//!   forced arms behind one inline confirm that states their cost first. Every
//!   sentence is a shared `fauna_client_admin` fold.
//! - **Legal takedown** (`admin-nest-takedown-*`) — the legal-compulsion
//!   console (`moderation.md` § Legal takedown → *Invocation surface*), every
//!   gating/wording decision the shared `fauna_client_moderation::takedown_form_view`
//!   fold; inline-confirm whose summary names verb + content + citation before
//!   dispatch (`ModerationClient::legal_takedown`).
//! - **Retire this server** (`admin-nest-retire-section`/`-button`) — opens the
//!   `nest_retire` page over the live session (`crate::wizard::nest_retire`;
//!   `nest-retirement.md`). Beside the reset and worded apart from it: reset
//!   wipes a box the admin keeps, retire destroys the box.
//! - **Factory reset** (`admin-factory-reset-section`/`-button`/`-confirm-button`)
//!   — the danger zone (`fauna.admin.factory_reset`), inline-confirm (sign-out shape).

use fauna_client_admin::IssuerForcedArm;
use fauna_i18n::strings::admin as t;
use fauna_i18n::strings::onboarding::nat_mode as nat_t;
use fauna_onboarding_machine::NodeMode;
use fauna_ui_ids as ids;

use super::{Action, AdminField, AdminState, FeatureLimitsRead, OauthKeysRead, SeedRotateConfirm};
use crate::element::{Element, Field, Gesture};
use crate::pages::Page;
use crate::wizard::localized;

/// The reports queue (`admin-nest-reports-section` — `moderation.md`
/// § User-initiated reporting → *Where it lands*), beside the takedown console
/// whose inbox it is. Paint only: every word is the shared `queue_row_view`
/// fold, and the row's three verbs are exactly the levers the goal doc names —
/// a door to the console, and two records. Rows paint FLAT and indexed, the
/// Moderation-page convention, so the e2e counts and clicks them by index.
fn reports_elements(state: &AdminState, els: &mut Vec<Element>) {
    use fauna_client_moderation::report;
    use fauna_protocol::moderation::AbuseReportOutcome;
    els.push(Element::label(
        ids::ADMIN_NEST_REPORTS_SECTION,
        t::nest_page::REPORTS_LABEL,
    ));
    els.push(Element::chrome(t::nest_page::REPORTS_DESC));
    if !state.reports_loaded {
        els.push(Element::chrome(t::nest_page::REPORTS_LOADING));
    } else if state.reports.is_empty() {
        els.push(Element::chrome(t::nest_page::REPORTS_EMPTY));
    }
    for entry in &state.reports {
        let view = report::queue_row_view(entry);
        let mut text = format!(
            "{} · {} {} · {} · {}",
            localized(&view.reason),
            view.subject_kind,
            view.subject_id,
            localized(&view.origin),
            crate::format::format_epoch_us(entry.created_at),
        );
        if let Some(note) = view.note.as_deref() {
            text.push_str(&format!(" — {note}"));
        }
        if let Some(excerpt) = view.excerpt.as_deref() {
            text.push_str(&format!(" — “{excerpt}”"));
        }
        els.push(
            Element::label(ids::ADMIN_NEST_REPORT_ITEM, text)
                .attr("subject", view.subject_id.clone())
                .attr("kind", view.subject_kind.clone()),
        );
        if view.can_open_takedown {
            els.push(Element::gesture_button(
                ids::ADMIN_NEST_REPORT_OPEN_TAKEDOWN_BUTTON,
                t::nest_page::REPORTS_OPEN_TAKEDOWN,
                true,
                Gesture::Admin(Action::OpenReportTakedown {
                    report_id: entry.report_id.clone(),
                }),
            ));
        }
        els.push(Element::gesture_button(
            ids::ADMIN_NEST_REPORT_ACTED_BUTTON,
            t::nest_page::REPORTS_ACTED,
            true,
            Gesture::Admin(Action::ResolveReport {
                report_id: entry.report_id.clone(),
                outcome: AbuseReportOutcome::Acted,
            }),
        ));
        els.push(Element::gesture_button(
            ids::ADMIN_NEST_REPORT_DISMISS_BUTTON,
            t::nest_page::REPORTS_DISMISS,
            true,
            Gesture::Admin(Action::ResolveReport {
                report_id: entry.report_id.clone(),
                outcome: AbuseReportOutcome::Dismissed,
            }),
        ));
    }
    if let Some(status) = state.reports_status.as_ref() {
        els.push(Element::chrome(status.clone()));
    }
}

/// The web-app origin section (`admin-nest-web-app-origin-*` — `admin.md` § N
/// Nest → *Web-app origin*), directly after NAT mode and mirroring its radio
/// shape. Paint only: every sentence is the shared `admin_web_app_origin_view`
/// fold; the radios' select is local, the save commits.
fn web_app_origin_elements(state: &AdminState, els: &mut Vec<Element>) {
    use fauna_client_admin::WebAppOrigin;
    let view = state.nest_snapshot.as_ref().map(|s| &s.web_app_origin);
    // Until the read lands, nothing is settable and neither radio is marked —
    // pre-marking bundled would state a choice the nest may not hold (the NAT
    // rule, `apps/tui.md` § Rendering → *Control vocabulary*, rule 1).
    let can_set = view.is_some_and(|v| v.can_set);
    let draft = state.web_app_origin_draft;
    let bundled = draft == Some(WebAppOrigin::Bundled);
    let central = draft == Some(WebAppOrigin::Central);
    els.push(Element::label(
        ids::ADMIN_NEST_WEB_APP_ORIGIN_SECTION,
        t::nest_page::WEB_APP_ORIGIN_LABEL,
    ));
    els.push(Element::chrome(t::nest_page::WEB_APP_ORIGIN_DESC));
    els.push(
        Element::radio_gesture(
            ids::ADMIN_NEST_WEB_APP_ORIGIN_BUNDLED_RADIO,
            t::nest_page::WEB_APP_ORIGIN_BUNDLED,
            bundled,
            Gesture::Admin(Action::SelectWebAppOrigin(WebAppOrigin::Bundled)),
        )
        .attr("state", if bundled { "on" } else { "off" })
        .enabled(can_set),
    );
    els.push(
        Element::radio_gesture(
            ids::ADMIN_NEST_WEB_APP_ORIGIN_CENTRAL_RADIO,
            view.map(|v| localized(&v.central_label))
                .unwrap_or_else(|| {
                    localized(&fauna_client_admin::AdminWebAppOriginView::default().central_label)
                }),
            central,
            Gesture::Admin(Action::SelectWebAppOrigin(WebAppOrigin::Central)),
        )
        .attr("state", if central { "on" } else { "off" })
        .enabled(can_set),
    );
    els.push(Element::gesture_button(
        ids::ADMIN_NEST_WEB_APP_ORIGIN_SAVE_BUTTON,
        t::nest_page::WEB_APP_ORIGIN_SAVE,
        can_set && draft.is_some(),
        Gesture::Admin(Action::SaveWebAppOrigin),
    ));
    els.push(Element::label(
        ids::ADMIN_NEST_WEB_APP_ORIGIN_STATUS,
        view.map(|v| localized(&v.status))
            .unwrap_or_else(|| t::nest_page::WEB_APP_ORIGIN_LOADING.to_string()),
    ));
    if let Some(v) = view {
        els.push(Element::chrome(localized(&v.scope)));
    }
}

pub(super) fn nest_elements(state: &AdminState) -> Vec<Element> {
    let reflective = state.nest_snapshot.as_ref();
    let pairing_enabled = reflective.map(|s| s.pairing_enabled).unwrap_or(false);
    let fronted = reflective.map(|s| s.fronted_by_router).unwrap_or(false);
    let os_updates = reflective
        .map(|s| s.os_security_updates_pending)
        .unwrap_or(0);
    let os_reboot = reflective.map(|s| s.os_reboot_pending).unwrap_or(false);

    let nat = state.nat_snapshot.as_ref();
    // Until the load-edge `hydrate` delivers the real `node_mode`, NEITHER radio
    // is marked — pre-marking Public would state a policy the nest may not hold
    // (`apps/tui.md` § Rendering → *Control vocabulary*, rule 1), and the reason
    // for the empty group rides `page_error` (the Privacy-page shape, walk I5).
    let nat_mode = nat.map(|s| s.selected_mode);
    let public = nat_mode == Some(NodeMode::Public);
    let private = nat_mode == Some(NodeMode::Private);
    let nat_submit_enabled = nat.map(|s| s.submit_enabled).unwrap_or(true);
    let nat_status = nat
        .map(|s| localized(&s.message))
        .unwrap_or_else(|| t::nest_page::NAT_MODE_LOADING.to_string());

    let mut els = vec![
        Element::label(ids::ADMIN_NEST_HEADING, t::nest_page::TITLE),
        Element::chrome(t::nest_page::DESCRIPTION),
        Element::gesture_button(
            ids::ADMIN_NAV_BACK,
            t::EXIT,
            true,
            Gesture::Nav(Page::Conversations),
        )
        .nav_back(),
        // Pairing — the admin master switch (non-optimistic: `state` and the
        // status text flip only once the nest write lands).
        Element::checkbox_gesture(
            ids::ADMIN_SERVICE_PAIRING_TOGGLE,
            t::services_page::PAIRING,
            pairing_enabled,
            Gesture::Admin(Action::TogglePairing),
        )
        .attr("state", if pairing_enabled { "on" } else { "off" }),
        Element::chrome(t::services_page::PAIRING_DESC),
        // The status text the e2e reads (`service_status_text("pairing")`) — the
        // shared Enabled/Disabled badge, never a hand-written string.
        Element::label(
            ids::ADMIN_SERVICE_PAIRING_STATUS,
            if pairing_enabled {
                t::services_page::ENABLED
            } else {
                t::services_page::DISABLED
            },
        ),
        // Serving port — the draft re-seeds from the persisted value on every
        // reflective snapshot (`super::apply_outcome`); behind the router it's inert
        // (the hint says 443), but ui.yaml still scopes the input to the page.
        Element::input(
            ids::ADMIN_NEST_SERVING_PORT_INPUT,
            state.serving_port_input.clone(),
            Field::Admin(AdminField::ServingPort),
        )
        .labelled(t::nest_page::SERVING_PORT_LABEL),
        Element::chrome(if fronted {
            t::nest_page::SERVING_PORT_FRONTED_HINT
        } else {
            t::nest_page::SERVING_PORT_DESC
        }),
        Element::gesture_button(
            ids::ADMIN_NEST_SERVING_PORT_SAVE_BUTTON,
            t::nest_page::SERVING_PORT_SAVE,
            true,
            Gesture::Admin(Action::SaveServingPort),
        ),
        // NAT mode — a one-of-N `Radio` pair (vocabulary rule 1), a save
        // (enabled per the machine's `submit_enabled`), and the machine-owned
        // status line. The radios' `select` is local (no network); save signs
        // the commit. Both unmarked until the mode has actually loaded.
        Element::radio_gesture(
            ids::ADMIN_NEST_NAT_MODE_PUBLIC_RADIO,
            nat_t::PUBLIC_LABEL,
            public,
            Gesture::Admin(Action::SelectNatMode(NodeMode::Public)),
        )
        .attr("state", if public { "on" } else { "off" }),
        Element::radio_gesture(
            ids::ADMIN_NEST_NAT_MODE_PRIVATE_RADIO,
            nat_t::PRIVATE_LABEL,
            private,
            Gesture::Admin(Action::SelectNatMode(NodeMode::Private)),
        )
        .attr("state", if private { "on" } else { "off" }),
        Element::gesture_button(
            ids::ADMIN_NEST_NAT_MODE_SAVE_BUTTON,
            t::nest_page::NAT_MODE_SAVE,
            nat_submit_enabled,
            Gesture::Admin(Action::SaveNatMode),
        ),
        Element::label(ids::ADMIN_NEST_NAT_MODE_STATUS, nat_status),
    ];

    web_app_origin_elements(state, &mut els);

    els.extend([
        // Host-OS maintenance — the always-present status line (the state→key
        // decision is the shared formatter), plus the count badge and restart-now
        // button that appear only when there is something pending.
        Element::label(
            ids::NEST_OS_MAINTENANCE_STATUS,
            localized(&fauna_core::format::os_maintenance_status_label(
                os_updates, os_reboot,
            )),
        ),
    ]);

    // `nest-os-updates-count` (optional) — the raw count, split out of the line so
    // the e2e asserts it without parsing the localized sentence.
    if os_updates > 0 {
        els.push(Element::label(
            ids::NEST_OS_UPDATES_COUNT,
            os_updates.to_string(),
        ));
    }
    // `nest-os-restart-now-button` (optional) — present only when a host reboot is
    // pending; expedites the nest-coordinated idle reboot.
    if os_reboot {
        els.push(Element::gesture_button(
            ids::NEST_OS_RESTART_NOW_BUTTON,
            t::nest_page::OS_RESTART_NOW,
            true,
            Gesture::Admin(Action::RestartHost),
        ));
    }

    // Declared region — the deployment's legal situs, the region tier's one human
    // choice. Every word below is the shared `admin_region_view` derivation; this
    // file decides nothing about the plane (priority #2 — six apps lift the same
    // fold, and what an admin is told about a tier that can BIND accounts here
    // must not differ per app).
    els.push(Element::label(
        ids::ADMIN_NEST_REGION_SECTION,
        t::nest_page::REGION_LABEL,
    ));
    els.push(Element::chrome(t::nest_page::REGION_DESC));
    let region = reflective.map(|s| &s.region);
    els.push(Element::label(
        ids::ADMIN_NEST_REGION_STATUS,
        region
            .map(|v| localized(&v.status))
            .unwrap_or_else(|| t::nest_page::REGION_NONE.to_string()),
    ));
    // Present only while a region is declared — before that there is no authority
    // channel to describe, and inventing a line about one would be a claim.
    if let Some(authority) = region.and_then(|v| v.authority.as_ref()) {
        els.push(Element::label(
            ids::ADMIN_NEST_REGION_AUTHORITY,
            localized(authority),
        ));
    }
    // The warning, only when the nest itself reports the channel unreached. Its
    // wording carries the fail posture: the rules already received stay in force,
    // so this is an "act when you can", never an outage.
    if let Some(stale) = region.and_then(|v| v.staleness.as_ref()) {
        els.push(Element::label(
            ids::ADMIN_NEST_REGION_STALENESS,
            localized(stale),
        ));
    }
    els.push(
        Element::input(
            ids::ADMIN_NEST_REGION_INPUT,
            state.region_input.clone(),
            Field::Admin(AdminField::Region),
        )
        .labelled(t::nest_page::REGION_PLACEHOLDER),
    );
    els.push(Element::gesture_button(
        ids::ADMIN_NEST_REGION_SAVE_BUTTON,
        t::nest_page::REGION_SAVE,
        true,
        Gesture::Admin(Action::SaveRegion),
    ));
    if region.map(|v| v.can_withdraw).unwrap_or(false) {
        els.push(Element::gesture_button(
            ids::ADMIN_NEST_REGION_WITHDRAW_BUTTON,
            t::nest_page::REGION_WITHDRAW,
            true,
            Gesture::Admin(Action::WithdrawRegion),
        ));
    }

    feature_limits_elements(state, &mut els);

    // Deployment identity — the rotation ceremony (`box-recovery.md` §
    // Deployment-seed rotation). Same inline-confirm shape as the factory reset
    // below, with one thing the reset does not have: the confirm's whole job is
    // to name **the set that will inherit** before dispatch (the doc's ordering
    // rule), so the roster listing is not decoration — it is the decision, and
    // the confirm stays disabled until it can be shown.
    els.push(Element::label(
        ids::ADMIN_NEST_SEED_ROTATE_SECTION,
        t::nest_page::ROTATE_SEED_LABEL,
    ));
    els.push(Element::chrome(t::nest_page::ROTATE_SEED_DESC));
    els.push(Element::gesture_button(
        ids::ADMIN_NEST_SEED_ROTATE_BUTTON,
        t::nest_page::ROTATE_SEED_BUTTON,
        true,
        Gesture::Admin(Action::OpenSeedRotateConfirm),
    ));
    if let Some(armed) = state.seed_rotate_confirm.as_ref() {
        els.push(Element::chrome(t::nest_page::ROTATE_SEED_CONFIRM_BODY));
        // One row per inheritor, painted only from a resolved roster. `Loading`
        // and `Failed` deliberately paint NO rows: an empty list beside a live
        // confirm would read as "nobody inherits", which is the one wrong thing
        // this surface must never say.
        let (rows, reason, enabled) = match armed {
            SeedRotateConfirm::Loading => (
                &[][..],
                Some(t::nest_page::ROTATE_SEED_ROSTER_LOADING.to_string()),
                false,
            ),
            SeedRotateConfirm::Failed(message) => (&[][..], Some(message.clone()), false),
            SeedRotateConfirm::Ready(view) => (
                view.inheritors.as_slice(),
                view.blocked_reason.as_ref().map(localized),
                view.can_confirm,
            ),
        };
        for (i, inheritor) in rows.iter().enumerate() {
            els.push(Element::label(
                format!("admin-nest-seed-rotate-roster-item-{i}"),
                inheritor.label.clone(),
            ));
        }
        if let Some(reason) = reason {
            els.push(Element::label(
                ids::ADMIN_NEST_SEED_ROTATE_ROSTER_REASON,
                reason,
            ));
        }
        els.push(Element::gesture_button(
            ids::ADMIN_NEST_SEED_ROTATE_CONFIRM_BUTTON,
            t::nest_page::ROTATE_SEED_CONFIRM_BUTTON,
            enabled,
            Gesture::Admin(Action::ConfirmSeedRotate),
        ));
        els.push(Element::gesture_button(
            ids::ADMIN_NEST_SEED_ROTATE_CANCEL_BUTTON,
            t::nest_page::ROTATE_SEED_CANCEL_BUTTON,
            true,
            Gesture::Admin(Action::CancelSeedRotate),
        ));
    }
    // The ceremony's own verdict, present only once one has been attempted. Not
    // `error-message`: the outcomes that matter most here are *successes with a
    // caveat* (a committed rotation whose bookkeeping write did not land), and
    // an error line would report the opposite of what happened.
    if let Some(status) = state.seed_rotate_status.as_ref() {
        els.push(Element::label(
            ids::ADMIN_NEST_SEED_ROTATE_STATUS,
            status.clone(),
        ));
    }

    oauth_elements(state, &mut els);

    // Legal takedown — the compulsion console (`moderation.md` § Legal takedown
    // → Invocation surface; the ruling). Paint only: every gating and
    // wording decision is the shared `takedown_form_view` fold — the arm button
    // is live exactly when the fold says so (a citation-less takedown is never
    // armable; a note-less RESTORE is), and the armed confirm renders the
    // CAPTURED fold, so it names exactly what a confirm will dispatch even if
    // the admin keeps typing.
    let takedown_view =
        fauna_client_moderation::takedown_form_view(&fauna_client_moderation::TakedownForm {
            content_id: state.takedown_content_id.clone(),
            content_type: if state.takedown_conversation {
                fauna_client_moderation::TakedownContentType::Conversation
            } else {
                fauna_client_moderation::TakedownContentType::Post
            },
            legal_reference: state.takedown_reference.clone(),
            restore: state.takedown_restore,
        });
    els.push(Element::label(
        ids::ADMIN_NEST_TAKEDOWN_SECTION,
        t::nest_page::TAKEDOWN_LABEL,
    ));
    els.push(Element::chrome(t::nest_page::TAKEDOWN_DESC));
    els.push(
        Element::input(
            ids::ADMIN_NEST_TAKEDOWN_CONTENT_ID_INPUT,
            state.takedown_content_id.clone(),
            Field::Admin(AdminField::TakedownContentId),
        )
        .labelled(t::nest_page::TAKEDOWN_CONTENT_ID_LABEL),
    );
    els.push(Element::radio_gesture(
        ids::ADMIN_NEST_TAKEDOWN_TYPE_POST_RADIO,
        t::nest_page::TAKEDOWN_TYPE_POST,
        !state.takedown_conversation,
        Gesture::Admin(Action::SetTakedownConversation(false)),
    ));
    els.push(Element::radio_gesture(
        ids::ADMIN_NEST_TAKEDOWN_TYPE_CONVERSATION_RADIO,
        t::nest_page::TAKEDOWN_TYPE_CONVERSATION,
        state.takedown_conversation,
        Gesture::Admin(Action::SetTakedownConversation(true)),
    ));
    els.push(
        Element::input(
            ids::ADMIN_NEST_TAKEDOWN_REFERENCE_INPUT,
            state.takedown_reference.clone(),
            Field::Admin(AdminField::TakedownReference),
        )
        .labelled(t::nest_page::TAKEDOWN_REFERENCE_LABEL),
    );
    els.push(
        Element::checkbox_gesture(
            ids::ADMIN_NEST_TAKEDOWN_RESTORE_CHECKBOX,
            t::nest_page::TAKEDOWN_RESTORE_LABEL,
            state.takedown_restore,
            Gesture::Admin(Action::ToggleTakedownRestore),
        )
        .attr("state", if state.takedown_restore { "on" } else { "off" }),
    );
    els.push(Element::gesture_button(
        ids::ADMIN_NEST_TAKEDOWN_BUTTON,
        localized(&takedown_view.arm_label),
        takedown_view.can_submit,
        Gesture::Admin(Action::OpenTakedownConfirm),
    ));
    if let Some(reason) = takedown_view.blocked_reason.as_ref() {
        // The disabled arm control owes its reason (walk I5's spirit), as chrome
        // — the reason is form guidance, not a page error.
        els.push(Element::chrome(localized(reason)));
    }
    if let Some(armed) = state.takedown_confirm.as_ref() {
        els.push(Element::label(
            ids::ADMIN_NEST_TAKEDOWN_CONFIRM_SUMMARY,
            localized(&armed.view.confirm_summary),
        ));
        els.push(Element::gesture_button(
            ids::ADMIN_NEST_TAKEDOWN_CONFIRM_BUTTON,
            localized(&armed.view.confirm_label),
            true,
            Gesture::Admin(Action::ConfirmTakedown),
        ));
        els.push(Element::gesture_button(
            ids::ADMIN_NEST_TAKEDOWN_CANCEL_BUTTON,
            t::nest_page::TAKEDOWN_CANCEL_BUTTON,
            true,
            Gesture::Admin(Action::CancelTakedown),
        ));
    }
    if let Some(status) = state.takedown_status.as_ref() {
        els.push(Element::label(
            ids::ADMIN_NEST_TAKEDOWN_STATUS,
            status.clone(),
        ));
    }

    reports_elements(state, &mut els);

    // Retire this server (`nest-retirement.md` § Layout & flow) — beside
    // Factory reset and worded sharply apart from it: reset wipes a box the
    // admin keeps, retire destroys the box. Opens the `nest_retire` page in the
    // wizard host over the live session (`crate::wizard::nest_retire`).
    els.push(Element::label(
        ids::ADMIN_NEST_RETIRE_SECTION,
        t::nest_page::RETIRE_TITLE,
    ));
    els.push(Element::chrome(t::nest_page::RETIRE_DESC));
    els.push(Element::gesture_button(
        ids::ADMIN_NEST_RETIRE_BUTTON,
        t::nest_page::RETIRE_BUTTON,
        true,
        Gesture::Retire(crate::wizard::nest_retire::RetireAction::OpenFromAdmin),
    ));

    // Factory reset — the danger zone. `admin-factory-reset-button` arms the inline
    // confirm (`admin-factory-reset-confirm-button`, present only while armed — the
    // sign-out-confirm shape); confirm mints+persists the code and dispatches.
    els.push(Element::label(
        ids::ADMIN_FACTORY_RESET_SECTION,
        t::settings_page::FACTORY_RESET_TITLE,
    ));
    els.push(Element::chrome(t::settings_page::FACTORY_RESET_DESC));
    els.push(Element::gesture_button(
        ids::ADMIN_FACTORY_RESET_BUTTON,
        t::settings_page::FACTORY_RESET_BUTTON,
        true,
        Gesture::Admin(Action::OpenFactoryResetConfirm),
    ));
    if state.factory_reset_confirming {
        els.push(Element::chrome(
            t::settings_page::FACTORY_RESET_CONFIRM_BODY,
        ));
        els.push(Element::gesture_button(
            ids::ADMIN_FACTORY_RESET_CONFIRM_BUTTON,
            t::settings_page::FACTORY_RESET_CONFIRM_BUTTON,
            true,
            Gesture::Admin(Action::ConfirmFactoryReset),
        ));
    }

    els
}

/// Feature limits (`admin-nest-feature-limits-*`) — the admin tier's authoring
/// host (`admin.md` § N Nest → Feature limits), directly after the declared
/// region: two tiers of one plane side by side. One row per gated feature the
/// nest carries — its name, the admin's AUTHORED document in words, and the edit
/// button that opens the shared `feature-policy-editor` right under that row.
/// Paint only: every word is `fauna_client_features`' (`authored_rows`, the
/// editor's view-model), and the gestures pick the admin tier's write kind.
fn feature_limits_elements(state: &AdminState, els: &mut Vec<Element>) {
    use fauna_i18n::strings::features as f;

    els.push(Element::label(
        ids::ADMIN_NEST_FEATURE_LIMITS_SECTION,
        f::ADMIN_SECTION_TITLE,
    ));
    els.push(Element::chrome(f::ADMIN_SECTION_DESC));
    let surface = match state.nest_snapshot.as_ref().map(|s| &s.feature_limits) {
        Some(FeatureLimitsRead::Ready(surface)) => surface,
        // Not read yet, or unreadable: the reason, never an empty list that
        // would read as "this nest gates nothing".
        Some(FeatureLimitsRead::Failed(reason)) => {
            els.push(Element::chrome(reason.clone()));
            return;
        }
        Some(FeatureLimitsRead::Unread) | None => return,
    };
    for (i, row) in surface.rows().iter().enumerate() {
        els.push(
            Element::label(ids::ADMIN_NEST_FEATURE_LIMITS_ROW, " ")
                .within(ids::ADMIN_NEST_FEATURE_LIMITS_ROW, i),
        );
        els.push(
            Element::label(ids::ADMIN_NEST_FEATURE_LIMITS_NAME, localized(&row.name))
                .within(ids::ADMIN_NEST_FEATURE_LIMITS_ROW, i),
        );
        els.push(
            Element::label(
                ids::ADMIN_NEST_FEATURE_LIMITS_SUMMARY,
                localized(&row.summary),
            )
            .within(ids::ADMIN_NEST_FEATURE_LIMITS_ROW, i),
        );
        els.push(
            Element::gesture_button(
                ids::ADMIN_NEST_FEATURE_LIMITS_EDIT_BUTTON,
                f::ADMIN_EDIT,
                true,
                Gesture::Admin(Action::OpenFeatureLimitEditor(row.feature.clone())),
            )
            .within(ids::ADMIN_NEST_FEATURE_LIMITS_ROW, i),
        );
        if let Some(editor) = state
            .feature_editor
            .as_ref()
            .filter(|e| e.feature_key() == row.feature)
        {
            els.extend(crate::feature_editor::editor_elements(
                editor,
                state.feature_editor_status.as_deref(),
                crate::feature_editor::EditorGestures {
                    on: Gesture::Admin(Action::FeatureLimitOn(true)),
                    off: Gesture::Admin(Action::FeatureLimitOn(false)),
                    save: Gesture::Admin(Action::SaveFeatureLimit),
                    remove: Gesture::Admin(Action::RemoveFeatureLimit),
                    cancel: Gesture::Admin(Action::CancelFeatureLimitEditor),
                    cell: |cell| Field::Admin(AdminField::FeatureLimitCell { cell }),
                },
            ));
        }
    }
}

/// Outside-app sign-in keys (`admin-nest-oauth-*`) — the nest-held OAuth issuer
/// key set and its refresh-token secret (`authorization-server.md` § The issuer
/// → *Two rotation arms*). Paint only: every sentence is a shared
/// `fauna_client_admin` fold, and the gestures refuse on the same
/// `super::oauth_keys` test the enabled flags below read.
fn oauth_elements(state: &AdminState, els: &mut Vec<Element>) {
    els.push(Element::label(
        ids::ADMIN_NEST_OAUTH_SECTION,
        t::nest_page::OAUTH_LABEL,
    ));
    els.push(Element::chrome(t::nest_page::OAUTH_DESC));

    // The key rows, painted ONLY from an answered read (the seed-rotate
    // roster's rule): "not asked yet" and "couldn't find out" get the reason
    // line instead, never an empty list that would read as "no keys".
    let keys = super::oauth_keys(state);
    match (keys, state.nest_snapshot.as_ref().map(|s| &s.oauth)) {
        (Some(view), _) => {
            // The countdown is the point of a retired key's line, so it is
            // counted against the wall clock at paint (`crate::format`'s
            // relative-time shape); the instant it counts to is the nest's own.
            let now = fauna_core::data::Timestamp::now_secs_or_zero();
            for (i, row) in view.keys.iter().enumerate() {
                els.push(Element::label(
                    format!("{}-{i}", ids::ADMIN_NEST_OAUTH_KEY_ITEM),
                    localized(&fauna_client_admin::issuer_key_row_label(row, now)),
                ));
            }
        }
        (None, Some(OauthKeysRead::Failed(reason))) => {
            els.push(Element::label(
                ids::ADMIN_NEST_OAUTH_KEY_REASON,
                reason.clone(),
            ));
        }
        (None, _) => {
            els.push(Element::label(
                ids::ADMIN_NEST_OAUTH_KEY_REASON,
                t::nest_page::OAUTH_KEYS_LOADING,
            ));
        }
    }

    // All three controls are live exactly when the set has answered and no call
    // is in flight — disabled, never hidden, beside the reason line otherwise.
    let live = keys.is_some() && !state.oauth_in_flight;
    // The ordinary arm states its cost beside itself: it has no confirm.
    if let Some(view) = keys {
        els.push(Element::chrome(localized(
            &fauna_client_admin::issuer_key_rotate_cost(view),
        )));
    }
    els.push(Element::gesture_button(
        ids::ADMIN_NEST_OAUTH_ROTATE_BUTTON,
        t::nest_page::OAUTH_ROTATE_BUTTON,
        live,
        Gesture::Admin(Action::RotateIssuerKey),
    ));
    els.push(Element::gesture_button(
        ids::ADMIN_NEST_OAUTH_FORCE_ROTATE_BUTTON,
        t::nest_page::OAUTH_FORCE_ROTATE_BUTTON,
        live,
        Gesture::Admin(Action::OpenOauthForcedConfirm(IssuerForcedArm::IssuerKey)),
    ));
    els.push(Element::gesture_button(
        ids::ADMIN_NEST_OAUTH_SECRET_FORCE_ROTATE_BUTTON,
        t::nest_page::OAUTH_SECRET_FORCE_ROTATE_BUTTON,
        live,
        Gesture::Admin(Action::OpenOauthForcedConfirm(
            IssuerForcedArm::SessionSecret,
        )),
    ));

    // The armed confirm renders the CAPTURED fold, and its confirm gesture
    // carries the arm it was painted for (the gesture refuses a mismatch).
    if let Some(armed) = state.oauth_confirm.as_ref() {
        els.push(Element::label(
            ids::ADMIN_NEST_OAUTH_CONFIRM_SUMMARY,
            localized(&armed.view.summary),
        ));
        els.push(Element::gesture_button(
            ids::ADMIN_NEST_OAUTH_CONFIRM_BUTTON,
            localized(&armed.view.confirm_label),
            true,
            Gesture::Admin(Action::ConfirmOauthForced(armed.arm)),
        ));
        els.push(Element::gesture_button(
            ids::ADMIN_NEST_OAUTH_CANCEL_BUTTON,
            t::nest_page::OAUTH_CANCEL_BUTTON,
            true,
            Gesture::Admin(Action::CancelOauthForced),
        ));
    }
    // The verdict, its own element — not `error-message`: every success here
    // has consequences worth words, and a failure must not claim nothing
    // changed.
    if let Some(status) = state.oauth_status.as_ref() {
        els.push(Element::label(ids::ADMIN_NEST_OAUTH_STATUS, status.clone()));
    }
}

/// The Nest sub-page's `error-message`: while the NAT snapshot has not loaded,
/// the mode radios mark NOTHING, and a one-of-N group with no marked member
/// owes the reason on the page's error line (walk I5; the Privacy-page shape).
pub(super) fn page_error(state: &AdminState) -> Option<String> {
    state
        .nat_snapshot
        .is_none()
        .then(|| t::nest_page::NAT_MODE_LOADING.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::{AdminPage, NestPageSnapshot};

    fn tagged_ids(els: &[Element]) -> Vec<&str> {
        els.iter()
            .map(|e| e.id.as_str())
            .filter(|id| !id.is_empty())
            .collect()
    }

    /// The reports queue: loading is not empty (no empty-state line until the
    /// read lands), each row carries its three levers (none to a takedown for
    /// an account), and open-takedown pre-fills the console without a citation.
    #[test]
    fn the_reports_queue_paints_its_states_and_prefills_the_console() {
        use fauna_protocol::moderation::{
            AbuseReportQueueEntry, AbuseReportReason, AbuseReportSubject,
        };
        let mut app = crate::app::tests::test_app();
        let texts = |app: &crate::app::App| -> Vec<String> {
            nest_elements(&app.admin)
                .into_iter()
                .map(|e| e.text)
                .collect()
        };
        assert!(!texts(&app).iter().any(|t| t == t::nest_page::REPORTS_EMPTY));
        app.admin.reports_loaded = true;
        assert!(texts(&app).iter().any(|t| t == t::nest_page::REPORTS_EMPTY));

        let entry = |id: &str, subject| AbuseReportQueueEntry {
            report_id: id.into(),
            subject,
            subject_actor: None,
            reason: AbuseReportReason::Spam,
            note: Some("look".into()),
            excerpt: None,
            reporter_handle: Some("alice".into()),
            origin_nest: None,
            created_at: 0,
            extra: Default::default(),
        };
        app.admin.reports = vec![
            entry(
                "r1",
                AbuseReportSubject::Post {
                    cid: "ab".repeat(32),
                },
            ),
            entry(
                "r2",
                AbuseReportSubject::Actor {
                    actor_id: "cd".repeat(32),
                },
            ),
        ];
        let els = nest_elements(&app.admin);
        let count = |id: &str| els.iter().filter(|e| e.id == id).count();
        assert_eq!(count("admin-nest-report-item"), 2);
        assert_eq!(count("admin-nest-report-open-takedown-button"), 1);
        assert_eq!(count("admin-nest-report-acted-button"), 2);
        assert_eq!(count("admin-nest-report-dismiss-button"), 2);
        assert!(!texts(&app).iter().any(|t| t == t::nest_page::REPORTS_EMPTY));

        app.admin.takedown_reference = "stale".into();
        let _ = crate::admin::apply_local(
            &mut app,
            Action::OpenReportTakedown {
                report_id: "r1".into(),
            },
        );
        assert_eq!(app.admin.takedown_content_id, "ab".repeat(32));
        assert!(!app.admin.takedown_conversation);
        assert!(app.admin.takedown_reference.is_empty());
        let arm = nest_elements(&app.admin)
            .into_iter()
            .find(|e| e.id == "admin-nest-takedown-button")
            .expect("arm button");
        assert!(!arm.enabled, "a pre-filled takedown still needs a citation");
    }

    /// The page paints its required ui.yaml `elements` in order (no optional
    /// elements when nothing is pending / the confirm is un-armed), and the pairing
    /// toggle's `state` attr + status text reflect the snapshot.
    #[test]
    fn nest_page_paints_the_required_elements_in_order() {
        let mut app = crate::app::tests::test_app();
        app.admin.sub = AdminPage::Nest;
        app.admin.nest_snapshot = Some(NestPageSnapshot {
            pairing_enabled: true,
            serving_port: 8443,
            ..Default::default()
        });
        app.admin.serving_port_input = "8443".to_string();

        let els = nest_elements(&app.admin);
        assert_eq!(
            tagged_ids(&els),
            vec![
                "admin-nest-heading",
                "admin-nav-back",
                "admin-service-pairing-toggle",
                "admin-service-pairing-status",
                "admin-nest-serving-port-input",
                "admin-nest-serving-port-save-button",
                "admin-nest-nat-mode-public-radio",
                "admin-nest-nat-mode-private-radio",
                "admin-nest-nat-mode-save-button",
                "admin-nest-nat-mode-status",
                "admin-nest-web-app-origin-section",
                "admin-nest-web-app-origin-bundled-radio",
                "admin-nest-web-app-origin-central-radio",
                "admin-nest-web-app-origin-save-button",
                "admin-nest-web-app-origin-status",
                "nest-os-maintenance-status",
                "admin-nest-region-section",
                "admin-nest-region-status",
                "admin-nest-region-input",
                "admin-nest-region-save-button",
                // Always present; its rows paint only from an answered read
                // (the default snapshot has not read the authored documents).
                "admin-nest-feature-limits-section",
                "admin-nest-seed-rotate-section",
                "admin-nest-seed-rotate-button",
                "admin-nest-oauth-section",
                // Not one of the required four, but the section always owes
                // either key rows or the reason for their absence — the
                // default snapshot has not read the set, so the reason paints.
                "admin-nest-oauth-key-reason",
                "admin-nest-oauth-rotate-button",
                "admin-nest-oauth-force-rotate-button",
                "admin-nest-oauth-secret-force-rotate-button",
                "admin-nest-takedown-section",
                "admin-nest-takedown-content-id-input",
                "admin-nest-takedown-type-post-radio",
                "admin-nest-takedown-type-conversation-radio",
                "admin-nest-takedown-reference-input",
                "admin-nest-takedown-restore-checkbox",
                "admin-nest-takedown-button",
                // The takedown console's inbox, right after it — its rows
                // paint only once the queue read lands.
                "admin-nest-reports-section",
                "admin-nest-retire-section",
                "admin-nest-retire-button",
                "admin-factory-reset-section",
                "admin-factory-reset-button",
            ]
        );

        let toggle = els
            .iter()
            .find(|e| e.id == "admin-service-pairing-toggle")
            .expect("pairing toggle painted");
        assert_eq!(
            toggle
                .attrs
                .iter()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.as_str()),
            Some("on"),
        );
        let status = els
            .iter()
            .find(|e| e.id == "admin-service-pairing-status")
            .expect("pairing status painted");
        assert_eq!(status.text, t::services_page::ENABLED);
        let port = els
            .iter()
            .find(|e| e.id == "admin-nest-serving-port-input")
            .expect("serving-port input painted");
        assert_eq!(port.text, "8443");
    }

    /// A disabled pairing snapshot flips both the toggle `state` and the status text.
    #[test]
    fn pairing_status_tracks_the_disabled_snapshot() {
        let mut app = crate::app::tests::test_app();
        app.admin.nest_snapshot = Some(NestPageSnapshot {
            pairing_enabled: false,
            ..Default::default()
        });
        let els = nest_elements(&app.admin);
        let toggle = els
            .iter()
            .find(|e| e.id == "admin-service-pairing-toggle")
            .unwrap();
        assert_eq!(
            toggle
                .attrs
                .iter()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.as_str()),
            Some("off"),
        );
        let status = els
            .iter()
            .find(|e| e.id == "admin-service-pairing-status")
            .unwrap();
        assert_eq!(status.text, t::services_page::DISABLED);
    }

    /// The os-maintenance count badge + restart-now button are painted only when
    /// something is pending; the confirm button only while the confirm is armed.
    #[test]
    fn optional_elements_gate_on_pending_state() {
        let mut app = crate::app::tests::test_app();
        // Updates pending + reboot pending + confirm armed → all three optionals.
        app.admin.nest_snapshot = Some(NestPageSnapshot {
            os_security_updates_pending: 3,
            os_reboot_pending: true,
            ..Default::default()
        });
        app.admin.factory_reset_confirming = true;
        let els = nest_elements(&app.admin);
        let ids = tagged_ids(&els);
        assert!(ids.contains(&"nest-os-updates-count"));
        assert!(ids.contains(&"nest-os-restart-now-button"));
        assert!(ids.contains(&"admin-factory-reset-confirm-button"));
        let count = els
            .iter()
            .find(|e| e.id == "nest-os-updates-count")
            .unwrap();
        assert_eq!(count.text, "3");

        // Nothing pending + un-armed confirm → none of the three.
        app.admin.nest_snapshot = Some(NestPageSnapshot::default());
        app.admin.factory_reset_confirming = false;
        let els = nest_elements(&app.admin);
        let ids = tagged_ids(&els);
        assert!(!ids.contains(&"nest-os-updates-count"));
        assert!(!ids.contains(&"nest-os-restart-now-button"));
        assert!(!ids.contains(&"admin-factory-reset-confirm-button"));
    }

    fn inheritor(label: &str) -> fauna_client_admin::SeedRotationInheritor {
        fauna_client_admin::SeedRotationInheritor {
            actor_id: vec![7u8; 32],
            label: label.to_string(),
        }
    }

    fn ready(view: fauna_client_admin::SeedRotationConfirmView) -> SeedRotateConfirm {
        SeedRotateConfirm::Ready(Box::new(view))
    }

    /// Un-armed, the ceremony shows exactly one control — arming it. None of the
    /// confirm's members exist, so nothing can be clicked into a rotation, and
    /// no status line appears before an attempt has been made.
    #[test]
    fn the_rotate_ceremony_paints_only_its_arming_button_until_armed() {
        let mut app = crate::app::tests::test_app();
        app.admin.nest_snapshot = Some(NestPageSnapshot::default());
        let els = nest_elements(&app.admin);
        let ids = tagged_ids(&els);
        assert!(ids.contains(&"admin-nest-seed-rotate-button"));
        for absent in [
            "admin-nest-seed-rotate-confirm-button",
            "admin-nest-seed-rotate-cancel-button",
            "admin-nest-seed-rotate-roster-reason",
            "admin-nest-seed-rotate-roster-item-0",
            "admin-nest-seed-rotate-status",
        ] {
            assert!(!ids.contains(&absent), "{absent} must not paint un-armed");
        }
    }

    /// The load-bearing property of this whole surface: the confirm is **never**
    /// enabled while the roster is unknown. Both unknown-states paint zero
    /// roster rows *and* a reason — an empty list beside a live confirm would
    /// read as "nobody inherits", which is the one thing this screen must never
    /// say (`box-recovery.md` § Deployment-seed rotation → *Ordering rule*).
    #[test]
    fn an_unresolved_roster_disables_the_confirm_and_paints_no_rows() {
        let mut app = crate::app::tests::test_app();
        app.admin.nest_snapshot = Some(NestPageSnapshot::default());

        for state in [
            SeedRotateConfirm::Loading,
            SeedRotateConfirm::Failed("nest unreachable".into()),
        ] {
            app.admin.seed_rotate_confirm = Some(state.clone());
            let els = nest_elements(&app.admin);
            let ids = tagged_ids(&els);
            assert!(
                !ids.iter()
                    .any(|id| id.starts_with("admin-nest-seed-rotate-roster-item")),
                "{state:?} must paint no roster rows"
            );
            assert!(ids.contains(&"admin-nest-seed-rotate-roster-reason"));
            let confirm = els
                .iter()
                .find(|e| e.id == "admin-nest-seed-rotate-confirm-button")
                .expect(
                    "the confirm is present but disabled — never absent, or a cancel-less \
                         surface would strand the admin",
                );
            assert!(
                !confirm.enabled,
                "{state:?} must not offer a live confirm — the roster is unknown"
            );
            // Cancel is always live: the admin must be able to back out of a
            // ceremony whose precondition never resolved.
            assert!(
                els.iter()
                    .find(|e| e.id == "admin-nest-seed-rotate-cancel-button")
                    .expect("cancel painted")
                    .enabled
            );
        }
    }

    /// A resolved roster paints one row per inheritor, in the nest's own order,
    /// with no reason line — and only then is the confirm live.
    #[test]
    fn a_resolved_roster_paints_a_row_per_inheritor_and_enables_the_confirm() {
        let mut app = crate::app::tests::test_app();
        app.admin.nest_snapshot = Some(NestPageSnapshot::default());
        app.admin.seed_rotate_confirm = Some(ready(fauna_client_admin::SeedRotationConfirmView {
            inheritors: vec![inheritor("root@fauna.test"), inheritor("aabbccddeeff…")],
            can_confirm: true,
            blocked_reason: None,
        }));

        let els = nest_elements(&app.admin);
        let ids = tagged_ids(&els);
        assert!(ids.contains(&"admin-nest-seed-rotate-roster-item-0"));
        assert!(ids.contains(&"admin-nest-seed-rotate-roster-item-1"));
        assert!(!ids.contains(&"admin-nest-seed-rotate-roster-item-2"));
        assert!(!ids.contains(&"admin-nest-seed-rotate-roster-reason"));
        assert_eq!(
            els.iter()
                .find(|e| e.id == "admin-nest-seed-rotate-roster-item-0")
                .unwrap()
                .text,
            "root@fauna.test"
        );
        assert!(
            els.iter()
                .find(|e| e.id == "admin-nest-seed-rotate-confirm-button")
                .unwrap()
                .enabled
        );
    }

    /// The shared fold can refuse a *resolved* roster too (an empty one is
    /// self-refuting — the admin reading this screen is themselves an admin).
    /// The page must honour `can_confirm`, not merely `Ready`-ness.
    #[test]
    fn a_resolved_but_refused_roster_still_disables_the_confirm() {
        let mut app = crate::app::tests::test_app();
        app.admin.nest_snapshot = Some(NestPageSnapshot::default());
        app.admin.seed_rotate_confirm = Some(ready(
            fauna_client_admin::seed_rotation_confirm_view(&Default::default(), &[]),
        ));

        let els = nest_elements(&app.admin);
        assert!(tagged_ids(&els).contains(&"admin-nest-seed-rotate-roster-reason"));
        assert!(
            !els.iter()
                .find(|e| e.id == "admin-nest-seed-rotate-confirm-button")
                .unwrap()
                .enabled
        );
    }

    /// The verdict line is its own element, not the page error — a rotation that
    /// committed but left its predecessor unmarked is a SUCCESS with a caveat,
    /// and reporting it on `error-message` would tell the admin the opposite of
    /// what happened to their nest.
    #[test]
    fn the_ceremony_verdict_rides_its_own_element_not_the_page_error() {
        let mut app = crate::app::tests::test_app();
        app.admin.nest_snapshot = Some(NestPageSnapshot::default());
        app.admin.nat_snapshot = Some(fauna_onboarding_machine::NatModeSnapshot {
            state: fauna_onboarding_machine::NatModeState::Choosing,
            selected_mode: NodeMode::Public,
            message: fauna_core::localized::LocalizedText::key("admin.nest_page.nat_mode_choosing"),
            submit_enabled: true,
        });
        app.admin.seed_rotate_status = Some(t::nest_page::ROTATE_SEED_DONE_UNMARKED.to_string());

        let els = nest_elements(&app.admin);
        assert_eq!(
            els.iter()
                .find(|e| e.id == "admin-nest-seed-rotate-status")
                .expect("verdict painted")
                .text,
            t::nest_page::ROTATE_SEED_DONE_UNMARKED
        );
        assert_eq!(page_error(&app.admin), None);
    }

    /// The takedown console's un-armed baseline: the seven form ids paint, the
    /// armed-confirm ids don't, and the arm button is DISABLED on the empty
    /// form (nothing to act on).
    #[test]
    fn the_takedown_console_paints_unarmed_and_refuses_an_empty_form() {
        let mut app = crate::app::tests::test_app();
        app.admin.nest_snapshot = Some(NestPageSnapshot::default());
        let els = nest_elements(&app.admin);
        let ids = tagged_ids(&els);
        for present in [
            "admin-nest-takedown-section",
            "admin-nest-takedown-content-id-input",
            "admin-nest-takedown-type-post-radio",
            "admin-nest-takedown-type-conversation-radio",
            "admin-nest-takedown-reference-input",
            "admin-nest-takedown-restore-checkbox",
            "admin-nest-takedown-button",
        ] {
            assert!(ids.contains(&present), "{present} must paint");
        }
        for absent in [
            "admin-nest-takedown-confirm-summary",
            "admin-nest-takedown-confirm-button",
            "admin-nest-takedown-cancel-button",
            "admin-nest-takedown-status",
        ] {
            assert!(!ids.contains(&absent), "{absent} must not paint un-armed");
        }
        assert!(
            !els.iter()
                .find(|e| e.id == "admin-nest-takedown-button")
                .unwrap()
                .enabled
        );
    }

    /// The arm button renders the shared guard: a citation-less TAKEDOWN is
    /// never armable, while the very same empty reference in RESTORE mode is —
    /// the wire guard's asymmetry, painted rather than re-derived per app.
    #[test]
    fn the_takedown_arm_button_follows_the_shared_guard() {
        let mut app = crate::app::tests::test_app();
        app.admin.nest_snapshot = Some(NestPageSnapshot::default());
        app.admin.takedown_content_id = "ab".repeat(32);

        let els = nest_elements(&app.admin);
        assert!(
            !els.iter()
                .find(|e| e.id == "admin-nest-takedown-button")
                .unwrap()
                .enabled,
            "a citation-less takedown must not be armable"
        );

        app.admin.takedown_restore = true;
        let els = nest_elements(&app.admin);
        assert!(
            els.iter()
                .find(|e| e.id == "admin-nest-takedown-button")
                .unwrap()
                .enabled,
            "a note-less restore must be armable"
        );
    }

    /// The armed confirm renders the CAPTURED summary — naming the content id
    /// and the citation, the decision surface a compulsory act must show before
    /// dispatch — plus a live confirm and a live cancel.
    #[test]
    fn an_armed_takedown_paints_the_named_confirm() {
        let mut app = crate::app::tests::test_app();
        app.admin.nest_snapshot = Some(NestPageSnapshot::default());
        let form = fauna_client_moderation::TakedownForm {
            content_id: "ab".repeat(32),
            content_type: fauna_client_moderation::TakedownContentType::Post,
            legal_reference: "Court order 42/2026".into(),
            restore: false,
        };
        let view = fauna_client_moderation::takedown_form_view(&form);
        app.admin.takedown_confirm = Some(crate::admin::ArmedTakedown { form, view });

        let els = nest_elements(&app.admin);
        let summary = els
            .iter()
            .find(|e| e.id == "admin-nest-takedown-confirm-summary")
            .expect("summary painted");
        assert!(
            summary.text.contains(&"ab".repeat(32)),
            "the confirm must name the content: {}",
            summary.text
        );
        assert!(
            summary.text.contains("Court order 42/2026"),
            "the confirm must name the citation: {}",
            summary.text
        );
        assert!(
            els.iter()
                .find(|e| e.id == "admin-nest-takedown-confirm-button")
                .unwrap()
                .enabled
        );
        assert!(
            els.iter()
                .find(|e| e.id == "admin-nest-takedown-cancel-button")
                .unwrap()
                .enabled
        );
    }

    /// The takedown verdict is its own element, not the page error — success is
    /// the common outcome and `error-message` would misreport it.
    #[test]
    fn the_takedown_verdict_rides_its_own_element() {
        let mut app = crate::app::tests::test_app();
        app.admin.nest_snapshot = Some(NestPageSnapshot::default());
        app.admin.takedown_status = Some(t::nest_page::TAKEDOWN_DONE.to_string());
        let els = nest_elements(&app.admin);
        assert_eq!(
            els.iter()
                .find(|e| e.id == "admin-nest-takedown-status")
                .expect("verdict painted")
                .text,
            t::nest_page::TAKEDOWN_DONE
        );
    }

    // ── Outside-app sign-in keys ────────────────────────────────────────────

    const OAUTH_BUTTONS: [&str; 3] = [
        "admin-nest-oauth-rotate-button",
        "admin-nest-oauth-force-rotate-button",
        "admin-nest-oauth-secret-force-rotate-button",
    ];

    fn with_oauth(keys: crate::admin::OauthKeysRead) -> NestPageSnapshot {
        NestPageSnapshot {
            oauth: keys,
            ..Default::default()
        }
    }

    /// The signer plus, optionally, one retired key still served for
    /// `retiring_for` more seconds (counted from the real clock — the paint
    /// counts against it).
    fn served(retiring_for: Option<i64>) -> crate::admin::OauthKeysRead {
        let mut keys = vec![fauna_client_admin::IssuerKeyRow {
            kid: "kid-new".into(),
            signing: true,
            retired_at: None,
            served_until: None,
        }];
        if let Some(left) = retiring_for {
            let until = fauna_core::data::Timestamp::now_secs_or_zero() + left;
            keys.push(fauna_client_admin::IssuerKeyRow {
                kid: "kid-old".into(),
                signing: false,
                retired_at: Some(until - 1_200),
                served_until: Some(until),
            });
        }
        crate::admin::OauthKeysRead::Ready(fauna_client_admin::IssuerKeyView {
            active_kid: "kid-new".into(),
            rotation_in_flight: keys.len() > 1,
            keys,
            retirement_horizon_secs: 1_200,
        })
    }

    fn enabled(els: &[Element], id: &str) -> bool {
        els.iter()
            .find(|e| e.id == id)
            .unwrap_or_else(|| panic!("{id} painted"))
            .enabled
    }

    /// Until the key set answers, the section paints the reason line and NO
    /// key rows — "not asked yet" and "couldn't find out" must not read as "no
    /// keys" — and all three controls are present but disabled (never
    /// hidden): the forced confirm could not name what it drops.
    #[test]
    fn the_sign_in_key_section_paints_a_reason_and_disabled_controls_until_read() {
        let mut app = crate::app::tests::test_app();
        for (keys, reason) in [
            (
                crate::admin::OauthKeysRead::Unread,
                t::nest_page::OAUTH_KEYS_LOADING.to_string(),
            ),
            (
                crate::admin::OauthKeysRead::Failed("unknown kind".into()),
                "unknown kind".to_string(),
            ),
        ] {
            app.admin.nest_snapshot = Some(with_oauth(keys));
            let els = nest_elements(&app.admin);
            let ids = tagged_ids(&els);
            assert!(
                !ids.iter()
                    .any(|id| id.starts_with("admin-nest-oauth-key-item")),
                "no key rows before the set answers"
            );
            assert_eq!(
                els.iter()
                    .find(|e| e.id == "admin-nest-oauth-key-reason")
                    .expect("reason painted")
                    .text,
                reason
            );
            for id in OAUTH_BUTTONS {
                assert!(!enabled(&els, id), "{id} must be disabled, not hidden");
            }
        }
    }

    /// An answered set paints one row per key, signer first, the retired key
    /// with its whole-minute countdown — and the controls come alive.
    #[test]
    fn an_answered_key_set_paints_a_row_per_key_and_live_controls() {
        let mut app = crate::app::tests::test_app();
        app.admin.nest_snapshot = Some(with_oauth(served(Some(600))));
        let els = nest_elements(&app.admin);
        let text = |id: &str| {
            els.iter()
                .find(|e| e.id == id)
                .unwrap_or_else(|| panic!("{id} painted"))
                .text
                .clone()
        };
        assert_eq!(
            text("admin-nest-oauth-key-item-0"),
            t::nest_page::oauth_key_signing("kid-new")
        );
        assert_eq!(
            text("admin-nest-oauth-key-item-1"),
            t::nest_page::oauth_key_retiring("kid-old", "10")
        );
        assert!(!tagged_ids(&els).contains(&"admin-nest-oauth-key-reason"));
        for id in OAUTH_BUTTONS {
            assert!(enabled(&els, id), "{id} must be live once the set answered");
        }
        // The ordinary arm has no confirm, so its cost is stated beside it.
        assert!(
            els.iter()
                .any(|e| e.text == t::nest_page::oauth_rotate_desc("20")),
            "the ordinary arm's cost must be painted"
        );
    }

    /// While a call is in flight every control desensitizes — each kind mints
    /// on the nest, and the ordinary arm has no confirm to disarm.
    #[test]
    fn an_in_flight_call_paints_the_controls_disabled() {
        let mut app = crate::app::tests::test_app();
        app.admin.nest_snapshot = Some(with_oauth(served(None)));
        app.admin.oauth_in_flight = true;
        let els = nest_elements(&app.admin);
        for id in OAUTH_BUTTONS {
            assert!(!enabled(&els, id), "{id} must be disabled mid-call");
        }
    }

    /// The armed confirm paints the CAPTURED cost, and its confirm gesture
    /// carries exactly the arm that was armed; un-armed, none of the confirm's
    /// members exist, and no verdict paints before a control was used.
    #[test]
    fn an_armed_forced_confirm_paints_its_captured_cost_and_arm() {
        let mut app = crate::app::tests::test_app();
        app.admin.nest_snapshot = Some(with_oauth(served(Some(600))));
        let els = nest_elements(&app.admin);
        for absent in [
            "admin-nest-oauth-confirm-summary",
            "admin-nest-oauth-confirm-button",
            "admin-nest-oauth-cancel-button",
            "admin-nest-oauth-status",
        ] {
            assert!(!tagged_ids(&els).contains(&absent), "{absent} un-armed");
        }

        let crate::admin::OauthKeysRead::Ready(view) = served(Some(600)) else {
            unreachable!()
        };
        app.admin.oauth_confirm = Some(crate::admin::ArmedOauthForced {
            arm: IssuerForcedArm::IssuerKey,
            view: fauna_client_admin::issuer_forced_confirm_view(IssuerForcedArm::IssuerKey, &view),
        });
        app.admin.oauth_status = Some("Replaced.".into());
        let els = nest_elements(&app.admin);
        let find = |id: &str| {
            els.iter()
                .find(|e| e.id == id)
                .unwrap_or_else(|| panic!("{id} painted"))
        };
        assert_eq!(
            find("admin-nest-oauth-confirm-summary").text,
            t::nest_page::oauth_force_rotate_confirm_many("2")
        );
        let confirm = find("admin-nest-oauth-confirm-button");
        assert_eq!(
            confirm.text,
            t::nest_page::OAUTH_FORCE_ROTATE_CONFIRM_BUTTON
        );
        assert!(matches!(
            confirm.role,
            crate::element::Role::Button(Gesture::Admin(Action::ConfirmOauthForced(
                IssuerForcedArm::IssuerKey
            )))
        ));
        assert!(find("admin-nest-oauth-cancel-button").enabled);
        assert_eq!(find("admin-nest-oauth-status").text, "Replaced.");
    }

    fn state_attr(e: &Element) -> Option<&str> {
        e.attrs
            .iter()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.as_str())
    }

    /// A web-app-origin-carrying snapshot; `None` is a nest predating the choice.
    fn with_origin(reply: Option<fauna_client_admin::WebAppOriginStatus>) -> NestPageSnapshot {
        NestPageSnapshot {
            web_app_origin: fauna_client_admin::admin_web_app_origin_view(reply.as_ref()),
            ..Default::default()
        }
    }

    fn origin_reply(
        mode: fauna_client_admin::WebAppOrigin,
        domain: Option<&str>,
    ) -> fauna_client_admin::WebAppOriginStatus {
        fauna_client_admin::WebAppOriginStatus::project(mode, domain)
    }

    /// Before the read lands nothing is marked or settable, and the status says
    /// it is loading — never a bundled pre-mark the nest may not hold.
    #[test]
    fn an_unloaded_web_app_origin_marks_neither_radio_and_cannot_save() {
        let mut app = crate::app::tests::test_app();
        app.admin.sub = AdminPage::Nest;
        let els = nest_elements(&app.admin);
        let find = |id: &str| els.iter().find(|e| e.id == id).expect(id).clone();
        for radio in [
            "admin-nest-web-app-origin-bundled-radio",
            "admin-nest-web-app-origin-central-radio",
        ] {
            assert!(!find(radio).enabled, "{radio} live before the read");
            assert_eq!(state_attr(&find(radio)), Some("off"));
        }
        assert!(!find("admin-nest-web-app-origin-save-button").enabled);
        assert_eq!(
            find("admin-nest-web-app-origin-status").text,
            t::nest_page::WEB_APP_ORIGIN_LOADING
        );
    }

    /// Central mode marks the central radio and the status names the nest's
    /// exact projected target; the central radio's label names the origin.
    #[test]
    fn central_mode_names_the_exact_target_users_are_sent_to() {
        use fauna_client_admin::WebAppOrigin;
        let mut app = crate::app::tests::test_app();
        app.admin.sub = AdminPage::Nest;
        app.admin.nest_snapshot = Some(with_origin(Some(origin_reply(
            WebAppOrigin::Central,
            Some("example.org"),
        ))));
        app.admin.web_app_origin_draft = Some(WebAppOrigin::Central);
        let els = nest_elements(&app.admin);
        let find = |id: &str| els.iter().find(|e| e.id == id).expect(id).clone();
        assert_eq!(
            state_attr(&find("admin-nest-web-app-origin-central-radio")),
            Some("on")
        );
        assert!(
            find("admin-nest-web-app-origin-central-radio")
                .text
                .contains("https://app.fauna.social")
        );
        assert!(find("admin-nest-web-app-origin-save-button").enabled);
        assert_eq!(
            find("admin-nest-web-app-origin-status").text,
            t::nest_page::web_app_origin_status_central(
                "https://app.fauna.social/app/?nest=example.org"
            )
        );
    }

    /// A nest predating the choice: both radios unmarked and disabled, the save
    /// dead, the status says why — and a radio press changes nothing.
    #[test]
    fn a_nest_predating_the_choice_paints_it_unsettable() {
        use fauna_client_admin::WebAppOrigin;
        let mut app = crate::app::tests::test_app();
        app.admin.sub = AdminPage::Nest;
        app.admin.nest_snapshot = Some(with_origin(None));
        assert!(
            super::super::apply_local(&mut app, Action::SelectWebAppOrigin(WebAppOrigin::Central))
                .is_none()
        );
        assert_eq!(app.admin.web_app_origin_draft, None);
        assert!(super::super::apply_local(&mut app, Action::SaveWebAppOrigin).is_none());
        let els = nest_elements(&app.admin);
        let find = |id: &str| els.iter().find(|e| e.id == id).expect(id).clone();
        assert!(!find("admin-nest-web-app-origin-central-radio").enabled);
        assert!(!find("admin-nest-web-app-origin-save-button").enabled);
        assert_eq!(
            find("admin-nest-web-app-origin-status").text,
            t::nest_page::WEB_APP_ORIGIN_STATUS_PREDATES
        );
    }

    /// Picking a radio is local: it moves the draft and dispatches nothing.
    #[test]
    fn picking_a_web_app_origin_radio_is_local() {
        use fauna_client_admin::WebAppOrigin;
        let mut app = crate::app::tests::test_app();
        app.admin.sub = AdminPage::Nest;
        app.admin.nest_snapshot =
            Some(with_origin(Some(origin_reply(WebAppOrigin::Bundled, None))));
        app.admin.web_app_origin_draft = Some(WebAppOrigin::Bundled);
        assert!(
            super::super::apply_local(&mut app, Action::SelectWebAppOrigin(WebAppOrigin::Central))
                .is_none()
        );
        assert_eq!(app.admin.web_app_origin_draft, Some(WebAppOrigin::Central));
    }

    /// A region-carrying snapshot for the region tests.
    fn with_region(reply: fauna_protocol::region::AdminRegionStatusReply) -> NestPageSnapshot {
        NestPageSnapshot {
            region: fauna_client_admin::admin_region_view(&reply),
            ..Default::default()
        }
    }

    fn region_code(code: &str) -> fauna_client_admin::RegionCode {
        fauna_client_admin::RegionCode::parse(code).expect("well-formed test code")
    }

    /// The undeclared deployment — the ratified fresh-install state — paints the
    /// status line and NOTHING conditional: no authority line (no channel exists
    /// to describe), no staleness warning, and no withdraw (nothing to withdraw).
    /// It must also not reach `error-message`: this state is conforming.
    #[test]
    fn undeclared_region_paints_no_conditional_members_and_no_error() {
        let mut app = crate::app::tests::test_app();
        app.admin.nest_snapshot = Some(NestPageSnapshot::default());
        let els = nest_elements(&app.admin);
        let ids = tagged_ids(&els);
        assert!(ids.contains(&"admin-nest-region-status"));
        assert!(!ids.contains(&"admin-nest-region-authority"));
        assert!(!ids.contains(&"admin-nest-region-staleness"));
        assert!(!ids.contains(&"admin-nest-region-withdraw-button"));
        let status = els
            .iter()
            .find(|e| e.id == "admin-nest-region-status")
            .expect("status painted");
        assert_eq!(status.text, t::nest_page::REGION_NONE);
    }

    /// A declaration paints the code, the authority line, and the withdraw — and
    /// the everywhere-today answer is the honest "no authority is enrolled",
    /// never silence, which would read as a broken feature.
    #[test]
    fn a_declared_region_paints_its_authority_line_and_withdraw() {
        let mut app = crate::app::tests::test_app();
        app.admin.nest_snapshot = Some(with_region(
            fauna_protocol::region::AdminRegionStatusReply {
                declared: Some(region_code("NO")),
                enrolled: false,
                ..Default::default()
            },
        ));
        let els = nest_elements(&app.admin);
        let ids = tagged_ids(&els);
        assert!(ids.contains(&"admin-nest-region-withdraw-button"));
        let authority = els
            .iter()
            .find(|e| e.id == "admin-nest-region-authority")
            .expect("a declared region owes an authority line");
        assert_eq!(authority.text, t::nest_page::REGION_NOT_ENROLLED);
        let status = els
            .iter()
            .find(|e| e.id == "admin-nest-region-status")
            .expect("status painted");
        assert!(
            status.text.contains("NO"),
            "the declared code must be visible, not merely held: {:?}",
            status.text
        );
    }

    /// The staleness warning paints only when the nest reports it — and it is a
    /// warning, not the page's `error-message`: a stale nest keeps enforcing the
    /// last-known-good document, so nothing has stopped working.
    #[test]
    fn staleness_paints_as_its_own_element_never_as_the_page_error() {
        let mut app = crate::app::tests::test_app();
        app.admin.nest_snapshot = Some(with_region(
            fauna_protocol::region::AdminRegionStatusReply {
                declared: Some(region_code("NO")),
                enrolled: true,
                stale: true,
                ..Default::default()
            },
        ));
        // A loaded NAT snapshot so `page_error`'s own (unrelated) arm is quiet.
        app.admin.nat_snapshot = Some(fauna_onboarding_machine::NatModeSnapshot {
            state: fauna_onboarding_machine::NatModeState::Choosing,
            selected_mode: NodeMode::Public,
            message: fauna_core::localized::LocalizedText::key("admin.nest_page.nat_mode_choosing"),
            submit_enabled: true,
        });
        let els = nest_elements(&app.admin);
        assert!(tagged_ids(&els).contains(&"admin-nest-region-staleness"));
        assert_eq!(
            page_error(&app.admin),
            None,
            "staleness is a warning, never the page error — the last rules stay in force"
        );
    }

    /// The draft mirrors nest state, so an admin sees what is declared rather
    /// than an empty box beside a declared region.
    #[test]
    fn the_region_draft_renders_the_persisted_declaration() {
        let mut app = crate::app::tests::test_app();
        app.admin.nest_snapshot = Some(NestPageSnapshot::default());
        app.admin.region_input = "EU".to_string();
        let els = nest_elements(&app.admin);
        let input = els
            .iter()
            .find(|e| e.id == "admin-nest-region-input")
            .expect("region input painted");
        assert_eq!(input.text, "EU");
    }

    /// The NAT radios reflect the snapshot's `selected_mode`: exactly one is
    /// checked, and it follows public/private.
    #[test]
    fn nat_radios_reflect_the_selected_mode() {
        use fauna_onboarding_machine::{NatModeSnapshot, NatModeState};

        let mut app = crate::app::tests::test_app();
        app.admin.nat_snapshot = Some(NatModeSnapshot {
            state: NatModeState::Choosing,
            selected_mode: NodeMode::Private,
            message: fauna_core::localized::LocalizedText::key("admin.nest_page.nat_mode_choosing"),
            submit_enabled: true,
        });
        let els = nest_elements(&app.admin);
        let checked = |id: &str| {
            els.iter()
                .find(|e| e.id == id)
                .map(|e| matches!(e.role, crate::element::Role::Radio { selected, .. } if selected))
                .unwrap()
        };
        assert!(!checked("admin-nest-nat-mode-public-radio"));
        assert!(checked("admin-nest-nat-mode-private-radio"));
    }

    /// Until the NAT snapshot hydrates, NEITHER radio is marked — pre-marking
    /// Public would state a policy the nest may not hold — and the page states
    /// the reason on its `error-message` (the Privacy-page shape, walk I5).
    #[test]
    fn an_unloaded_nat_mode_marks_neither_radio_and_states_why() {
        let mut app = crate::app::tests::test_app();
        app.admin.sub = AdminPage::Nest;
        assert!(app.admin.nat_snapshot.is_none());

        let els = nest_elements(&app.admin);
        // Scoped to the NAT-mode group: the takedown TYPE radios on the same
        // page carry a legitimate default (post) — a content id is one exact
        // thing, not an unknown policy.
        let marked = els
            .iter()
            .filter(|e| e.id.starts_with("admin-nest-nat-mode"))
            .filter(|e| matches!(e.role, crate::element::Role::Radio { selected, .. } if selected))
            .count();
        assert_eq!(marked, 0, "no guessed default while the mode is unknown");
        assert_eq!(
            crate::admin::page_error(&app.admin).as_deref(),
            Some(t::nest_page::NAT_MODE_LOADING),
            "the empty group's reason rides the page error"
        );
    }

    // ── Feature limits — the admin authoring host ────────────────────────

    /// A snapshot whose admin-tier authored read answered: every member, with
    /// `p2p-share` carrying an authored "5 uses a day".
    fn with_feature_limits() -> NestPageSnapshot {
        use fauna_client_features::{AuthoredPolicyItem, GatedFeature};
        use fauna_core::feature_gate::{Availability, FeaturePolicy, Window, WindowedBounds};
        let item = |feature: GatedFeature, policy: Option<FeaturePolicy>| AuthoredPolicyItem {
            feature,
            policy,
            unreadable: false,
            ceiling: fauna_client_features::test_fixtures::item(feature, &[]).policy,
            extra: Default::default(),
        };
        NestPageSnapshot {
            feature_limits: crate::admin::FeatureLimitsRead::Ready(
                fauna_client_features::AuthoredSurface {
                    items: vec![
                        item(GatedFeature::Payments, None),
                        item(GatedFeature::Zaps, None),
                        item(
                            GatedFeature::P2pShare,
                            Some(FeaturePolicy {
                                availability: Availability::Limit,
                                operations: WindowedBounds::at(Window::Day, 5),
                                ..FeaturePolicy::NO_OPINION
                            }),
                        ),
                    ],
                    ..Default::default()
                },
            ),
            ..Default::default()
        }
    }

    fn scoped_text<'a>(els: &'a [Element], id: &str, container: &str, i: usize) -> &'a str {
        els.iter()
            .find(|e| e.id == id && e.path.iter().any(|s| s.0 == container && s.1 == i))
            .map(|e| e.text.as_str())
            .unwrap_or_default()
    }

    /// One row per carried member, each naming the ADMIN's authored document —
    /// "No limit set" for no document, never the effective meet.
    #[test]
    fn feature_limit_rows_paint_the_authored_documents_in_words() {
        let mut app = crate::app::tests::test_app();
        app.admin.nest_snapshot = Some(with_feature_limits());
        let els = nest_elements(&app.admin);
        let row = "admin-nest-feature-limits-row";
        assert_eq!(
            scoped_text(&els, "admin-nest-feature-limits-name", row, 2),
            fauna_i18n::strings::features::NAME_P2P_SHARE
        );
        assert_eq!(
            scoped_text(&els, "admin-nest-feature-limits-summary", row, 2),
            fauna_i18n::strings::features::AUTHORED_LIMITED_ONE
        );
        assert_eq!(
            scoped_text(&els, "admin-nest-feature-limits-summary", row, 0),
            fauna_i18n::strings::features::AUTHORED_NONE
        );
        assert!(
            !tagged_ids(&els).contains(&"feature-policy-editor"),
            "closed"
        );
    }

    /// The edit button opens the SHARED editor at the admin tier, seeded from the
    /// authored document; its writes declare the admin kind the offline gate
    /// reads, and cancel is local.
    #[test]
    fn the_edit_button_opens_the_shared_editor_seeded_from_the_authored_document() {
        let mut app = crate::app::tests::test_app();
        app.admin.nest_snapshot = Some(with_feature_limits());
        let op =
            super::super::apply_local(&mut app, Action::OpenFeatureLimitEditor("p2p-share".into()));
        assert!(op.is_none(), "opening is local");
        let els = nest_elements(&app.admin);
        let ids = tagged_ids(&els);
        for id in [
            "feature-policy-editor",
            "feature-policy-editor-title",
            "feature-policy-editor-on-radio",
            "feature-policy-editor-off-radio",
            "feature-policy-editor-cell",
            "feature-policy-editor-cell-input",
            "feature-policy-editor-save-button",
            "feature-policy-editor-remove-button",
            "feature-policy-editor-cancel-button",
        ] {
            assert!(ids.contains(&id), "{id} missing: {ids:?}");
        }
        assert_eq!(
            scoped_text(
                &els,
                "feature-policy-editor-cell-input",
                "feature-policy-editor-cell",
                0
            ),
            "5"
        );
        assert_eq!(
            Action::SaveFeatureLimit.wire_kind(),
            Some("fauna.features.policy.update")
        );
        assert_eq!(
            Action::RemoveFeatureLimit.wire_kind(),
            Some("fauna.features.policy.update")
        );
        assert_eq!(Action::CancelFeatureLimitEditor.wire_kind(), None);
    }

    /// An unparseable cell goes to `error-message` and dispatches nothing.
    #[test]
    fn an_unparseable_cell_refuses_before_any_dispatch() {
        let mut app = crate::app::tests::test_app();
        app.admin.nest_snapshot = Some(with_feature_limits());
        let _ =
            super::super::apply_local(&mut app, Action::OpenFeatureLimitEditor("p2p-share".into()));
        super::super::set_field(
            &mut app.admin,
            AdminField::FeatureLimitCell { cell: 0 },
            "lots".into(),
        );
        let op = super::super::apply_local(&mut app, Action::SaveFeatureLimit);
        assert!(op.is_none());
        assert!(
            app.errors
                .get(&crate::pages::Page::Admin)
                .is_some_and(|e| e.contains("lots")),
            "{:?}",
            app.errors.get(&crate::pages::Page::Admin)
        );
    }
}
