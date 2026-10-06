//! The admin Users sub-page (`admin-users`) — the user-administration hub
//! (`admin.md` § 2 Users). Four sections on one page, all framed around assigning a
//! *tier* (the tier IS the quota): **Pending requests** (approve/deny invite
//! requests), **Registration** (the nest's registration posture), **Invite** (mint
//! invite codes), and **Users** (the user list — tier change, eviction/suspension/
//! restore, the read-only serving audit, pagination).
//!
//! Drives **directly** off the shared `AdminClient` (`fauna.admin.users.*` /
//! `invite_codes.*` / `invite_requests.*` / `set_registration_mode`) — there is no
//! users *machine* (`fauna-client-admin` is a thin one-method-per-kind client), so
//! the shell (`super`) owns the reads/writes and this file is paint only, mirroring
//! `settings.rs`. The whole page rides one [`super::UsersSnapshot`] re-read on entry
//! and after every mutation (`admin.md` § Persistence — no client-side cache).
//!
//! Errors route to the **page-scoped** `admin-users-action-error` (all three
//! sections; `admin.md` § Errors), NOT the app-wide `error-message`.
//!
//! **Load-bearing e2e contract** (`actions/admin.py`): `user-row` / `user-actor-id`
//! / `admin-users-tier-select` are read **FLAT** (occurrence = row order); the
//! `admin-users-mail-serving-status` / evict / suspend / cancel controls are read
//! **scoped under `user-row[i]`** — the `settings.rs` anchor-plus-`.within` idiom.
//! The tier picker is a `Role::Select` (a cycle Button silently fails on tui —
//! `_pick_tier`'s fallback keys on the linux `"not a selector"` string, which tui
//! doesn't emit), and its select applies `users_update` LIVE + re-reads.

use fauna_client_admin::{
    RegistrationMode, admin_user_row_controls, age_band_options, registration_mode_options,
};
use fauna_i18n::strings::admin as t;
use fauna_ui_ids as ids;

use super::{
    Action, AdminField, AdminState, GUARDIAN_NONE_VALUE, USERS_PAGE_SIZE, UsersSnapshot,
    UsersState, guardian_draft_names_someone,
};
use crate::element::{Element, Field, Gesture, SelectTarget};
use crate::pages::Page;

pub(super) fn users_elements(state: &AdminState) -> Vec<Element> {
    let users = &state.users;

    let mut els = vec![
        Element::label(ids::ADMIN_USERS_HEADING, t::users_page::TITLE),
        Element::gesture_button(
            ids::ADMIN_NAV_BACK,
            t::EXIT,
            true,
            Gesture::Nav(Page::Conversations),
        )
        .nav_back(),
    ];

    // Page-scoped action error — painted ONLY when a mutation left one (the
    // empty-doesn't-register discipline), so `is_visible` is false on a clean page
    // and the app-wide `error-message` stays untouched (`admin.md` § Errors).
    if let Some(err) = users.action_error.as_deref().filter(|e| !e.is_empty()) {
        els.push(Element::label(ids::ADMIN_USERS_ACTION_ERROR, err));
    }

    // The sections don't register until the nav-edge fetch lands (the stat-card
    // `None`-while-empty shape — the e2e `wait_for`s real data, not a blank page).
    let Some(snapshot) = users.snapshot.as_ref() else {
        return els;
    };

    // ui.yaml order: pending requests → registration → admit → invite → the
    // user list.
    els.extend(requests_section(users, snapshot));
    els.extend(registration_section(users, snapshot));
    els.extend(admit_section(users, snapshot));
    els.extend(invite_section(users, snapshot));
    els.extend(user_list_section(users, snapshot));
    els.extend(pending_section(snapshot));
    els
}

/// Section 6 — pending admin actions (`admin.md` § Pending admin actions): the
/// STANDING co-admin approve / veto surface over every account's still-pending
/// delayed admin action. The container's text answers honestly in the
/// settings-page's three states (the snapshot is loaded whenever this paints,
/// so it is the empty-state line or the counted title); one row per pending
/// action with the shared description plus who scheduled it, when it runs,
/// and its approvals readout; approve and cancel are one click each. Row
/// children ride the `.within(row, i)` idiom of the settings section so both
/// flat and scoped queries resolve.
fn pending_section(snapshot: &UsersSnapshot) -> Vec<Element> {
    let rows: Vec<_> = snapshot
        .pending_actions
        .iter()
        .filter(|a| a.status == "pending")
        .collect();
    let mut els = vec![Element::label(
        ids::ADMIN_USERS_PENDING_SECTION,
        if rows.is_empty() {
            t::users_page::PENDING_NONE.to_string()
        } else {
            t::users_page::pending_count(&rows.len().to_string())
        },
    )];
    for (i, action) in rows.iter().enumerate() {
        let description = format!(
            "{} {}",
            fauna_protocol::pending_actions::describe_pending_action(
                &action.action_type,
                action.target.as_deref(),
            ),
            t::users_page::pending_by(&fauna_core::format::short_id(&hex::encode(
                &action.actor_id
            ))),
        );
        els.push(
            Element::label(ids::ADMIN_PENDING_ACTION_ITEM, " ")
                .within(ids::ADMIN_PENDING_ACTION_ITEM, i),
        );
        els.push(
            Element::label(ids::ADMIN_PENDING_ACTION_DESCRIPTION, description)
                .within(ids::ADMIN_PENDING_ACTION_ITEM, i),
        );
        els.push(
            Element::label(
                ids::ADMIN_PENDING_ACTION_EXECUTE_AFTER,
                fauna_i18n::strings::settings::pending_actions::applies(
                    &fauna_core::format::format_unix_local(action.execute_after),
                ),
            )
            .within(ids::ADMIN_PENDING_ACTION_ITEM, i),
        );
        els.push(
            Element::label(
                ids::ADMIN_PENDING_ACTION_APPROVALS,
                t::users_page::pending_approvals(
                    &action.approvals.len().to_string(),
                    &action.requires_quorum.to_string(),
                ),
            )
            .within(ids::ADMIN_PENDING_ACTION_ITEM, i),
        );
        els.push(
            Element::gesture_button(
                ids::ADMIN_PENDING_ACTION_APPROVE_BUTTON,
                t::users_page::PENDING_APPROVE,
                true,
                Gesture::Admin(Action::ApprovePending { id: action.id }),
            )
            .within(ids::ADMIN_PENDING_ACTION_ITEM, i),
        );
        els.push(
            Element::gesture_button(
                ids::ADMIN_PENDING_ACTION_CANCEL_BUTTON,
                fauna_i18n::strings::settings::pending_actions::CANCEL,
                true,
                Gesture::Admin(Action::CancelPending { id: action.id }),
            )
            .within(ids::ADMIN_PENDING_ACTION_ITEM, i),
        );
    }
    els
}

/// Section 1 — pending invite-requests (`admin.md` § 2 → Section 1): one row per
/// PENDING request (`is_pending()` — decided rows aren't shown, keeping the count
/// clean and the controls unreachable) with the approve-at tier + guardian pickers
/// and approve/deny + reason. All row children are FLAT indexed leaf ids (the driver
/// reads them by occurrence, `actions/admin.py`); the row identity is baked into each
/// picker/button/field so the flat occurrence resolves the right request.
fn requests_section(state: &UsersState, snapshot: &UsersSnapshot) -> Vec<Element> {
    let mut els = vec![Element::label(
        ids::ADMIN_USERS_REQUESTS_SECTION,
        t::users_page::SECTION_REQUESTS,
    )];
    for (i, req) in snapshot.invite_requests.iter().enumerate() {
        if !req.is_pending() {
            continue;
        }
        els.push(Element::label(
            ids::INVITE_REQUEST_ROW_ACTOR,
            fauna_core::format::short_id(&fauna_core::format::hex_full(req.actor_id.as_ref())),
        ));
        els.push(Element::label(
            ids::INVITE_REQUEST_ROW_HANDLE,
            req.handle.clone(),
        ));
        els.push(Element::label(
            ids::INVITE_REQUEST_ROW_MESSAGE,
            req.message.clone(),
        ));
        els.push(Element::select(
            ids::INVITE_REQUEST_ROW_TIER_SELECT,
            state
                .request_tier_drafts
                .get(i)
                .cloned()
                .unwrap_or_default(),
            SelectTarget::RequestTier { row: i },
            snapshot.tiers.clone(),
        ));
        let guardian_draft = state
            .request_guardian_drafts
            .get(i)
            .map(String::as_str)
            .unwrap_or(GUARDIAN_NONE_VALUE);
        els.push(guardian_picker(
            "invite-request-row-guardian-select",
            guardian_draft,
            SelectTarget::RequestGuardian { row: i },
            snapshot,
        ));
        // The applicant's claim — total: absence IS the signal the admitting
        // admin reads (D6), so a request with no claim says so.
        els.push(Element::label(
            ids::INVITE_REQUEST_ROW_AGE_CLAIM,
            fauna_protocol::age::age_claim_label(
                req.age_band.as_deref(),
                req.age_band_provenance.as_deref(),
            )
            .resolve_nested(fauna_i18n::strings::lookup),
        ));
        els.push(age_band_picker(
            ids::INVITE_REQUEST_ROW_AGE_BAND_SELECT,
            state
                .request_age_band_drafts
                .get(i)
                .map(String::as_str)
                .unwrap_or(fauna_client_admin::AGE_BAND_NOT_SET_VALUE),
            SelectTarget::RequestAgeBand { row: i },
            guardian_draft_names_someone(guardian_draft),
        ));
        els.push(Element::gesture_button(
            ids::INVITE_REQUEST_ROW_APPROVE_BUTTON,
            t::invite_requests_page::APPROVE,
            true,
            Gesture::Admin(Action::ApproveRequest { row: i }),
        ));
        els.push(Element::gesture_button(
            ids::INVITE_REQUEST_ROW_DENY_BUTTON,
            t::invite_requests_page::DENY,
            true,
            Gesture::Admin(Action::DenyRequest { row: i }),
        ));
        els.push(
            Element::input(
                ids::INVITE_REQUEST_ROW_DENY_REASON_FIELD,
                state
                    .request_deny_reasons
                    .get(i)
                    .cloned()
                    .unwrap_or_default(),
                Field::Admin(AdminField::RequestDenyReason { row: i }),
            )
            .labelled(t::invite_requests_page::DENY_REASON_PLACEHOLDER),
        );
    }
    els
}

/// Section 2 — registration posture (`admin.md` § 2 → Section 2): the mode picker
/// (wire values from the shared `registration_mode_options`), the free-tier ceiling,
/// and one combined save. A posture this client doesn't recognize renders read-only
/// (never coerced — a save would overwrite the nest's real posture). Not e2e-tested
/// on tui (the posture test is deselected), but rendered for parity.
fn registration_section(state: &UsersState, snapshot: &UsersSnapshot) -> Vec<Element> {
    let mut els = vec![Element::label(
        ids::ADMIN_USERS_REGISTRATION_SECTION,
        t::users_page::SECTION_REGISTRATION,
    )];
    let known = snapshot
        .registration_mode
        .as_deref()
        .is_some_and(|m| RegistrationMode::from_wire_str(m).is_some());
    if !known {
        // Unknown/absent posture — a read-only explainer, no picker/save (never coerce).
        let raw = snapshot.registration_mode.clone().unwrap_or_default();
        els.push(Element::label(
            ids::ADMIN_USERS_REGISTRATION_MODE_SELECT,
            t::users_page::registration_mode_unknown(&raw),
        ));
        return els;
    }
    let options: Vec<String> = registration_mode_options()
        .into_iter()
        .map(|o| o.value)
        .collect();
    els.push(Element::select(
        ids::ADMIN_USERS_REGISTRATION_MODE_SELECT,
        state.registration_mode_draft.clone(),
        SelectTarget::RegistrationMode,
        options,
    ));
    els.push(
        Element::input(
            ids::ADMIN_USERS_MAX_FREE_USERS_INPUT,
            state.max_free_users_input.clone(),
            Field::Admin(AdminField::MaxFreeUsers),
        )
        .labelled(t::users_page::MAX_FREE_USERS_LABEL),
    );
    // The age require-knob (`family-safety.md` § The account age band D5+D6)
    // — a draft in this section's one-gesture save, the family page's toggle
    // idiom (`state` attr mirrors the checkbox for the driver).
    els.push(
        Element::checkbox_gesture(
            ids::ADMIN_USERS_REGISTRATION_AGE_VERIFICATION_TOGGLE,
            t::users_page::AGE_VERIFICATION_REQUIRED_LABEL,
            state.age_verification_draft,
            Gesture::Admin(Action::SetAgeVerificationRequired(
                !state.age_verification_draft,
            )),
        )
        .attr(
            "state",
            if state.age_verification_draft {
                "on"
            } else {
                "off"
            },
        ),
    );
    els.push(Element::gesture_button(
        ids::ADMIN_USERS_REGISTRATION_SAVE_BUTTON,
        t::users_page::REGISTRATION_SAVE,
        true,
        Gesture::Admin(Action::SaveRegistration),
    ));
    els
}

/// The Admit section — direct admission, the third account-creation path
/// (`public-mode.md` § Registration & Identity; user-approved IDs 2026-08-15;
/// tui leads, the other six apps' legs are entrusted). The admin types a known
/// actor id, names the handle the actor is admitted under (there is no
/// set-later — `clear_handle` can only strip one; a blank handle admits the
/// deliberate handle-less state, which cannot send deployment-domain mail),
/// picks a tier ("admission is always choosing a tier"), and admits — one
/// `fauna.admin.users.create` call. On success the users list refetches and
/// the new row is the feedback; errors land on `admin-users-action-error`.
fn admit_section(state: &UsersState, snapshot: &UsersSnapshot) -> Vec<Element> {
    vec![
        Element::label(ids::ADMIN_USERS_ADMIT_SECTION, t::users_page::SECTION_ADMIT),
        Element::input(
            ids::ADMIN_USERS_ADMIT_ACTOR_INPUT,
            state.admit_actor_input.clone(),
            Field::Admin(AdminField::AdmitActor),
        )
        .labelled(t::users_page::ADMIT_ACTOR_LABEL),
        Element::input(
            ids::ADMIN_USERS_ADMIT_HANDLE_INPUT,
            state.admit_handle_input.clone(),
            Field::Admin(AdminField::AdmitHandle),
        )
        .labelled(t::users_page::ADMIT_HANDLE_LABEL),
        Element::select(
            ids::ADMIN_USERS_ADMIT_TIER_SELECT,
            state.admit_tier_draft.clone(),
            SelectTarget::AdmitTier,
            snapshot.tiers.clone(),
        ),
        Element::gesture_button(
            ids::ADMIN_USERS_ADMIT_BUTTON,
            t::users_page::ADMIT_BUTTON,
            true,
            Gesture::Admin(Action::AdmitUser),
        ),
    ]
}

/// Section 3 — invite-code minting (`admin.md` § 2 → Section 3): the create form
/// (revealed by `create-invite-code-btn`, minted by `create-invite-confirm-btn`, no
/// code field — the nest mints), the copyable minted token, and the existing-codes
/// list. The `admin-settings-invite-create-form` container view is realized via its
/// leaves (a terminal has no form chrome — the dashboard stat-card precedent).
fn invite_section(state: &UsersState, snapshot: &UsersSnapshot) -> Vec<Element> {
    let mut els = vec![Element::label(
        ids::ADMIN_USERS_INVITE_SECTION,
        t::users_page::SECTION_INVITE,
    )];

    if state.invite_form_open {
        els.push(Element::select(
            ids::ADMIN_SETTINGS_TIER_SELECT,
            state.invite_tier_draft.clone(),
            SelectTarget::InviteTier,
            snapshot.tiers.clone(),
        ));
        els.push(
            Element::input(
                ids::ADMIN_SETTINGS_MAX_USES_INPUT,
                state.invite_uses_input.clone(),
                Field::Admin(AdminField::InviteMaxUses),
            )
            .labelled(t::settings_page::MAX_USES),
        );
        els.push(guardian_picker(
            "admin-users-invite-guardian-select",
            &state.invite_guardian_draft,
            SelectTarget::InviteGuardian,
            snapshot,
        ));
        els.push(age_band_picker(
            ids::ADMIN_USERS_INVITE_AGE_BAND_SELECT,
            &state.invite_age_band_draft,
            SelectTarget::InviteAgeBand,
            guardian_draft_names_someone(&state.invite_guardian_draft),
        ));
        els.push(Element::gesture_button(
            ids::CREATE_INVITE_CONFIRM_BTN,
            t::settings_page::CREATE_CODE,
            true,
            Gesture::Admin(Action::ConfirmInvite),
        ));
        els.push(Element::gesture_button(
            ids::ADMIN_SETTINGS_INVITE_CANCEL_BUTTON,
            t::users_page::CANCEL,
            true,
            Gesture::Admin(Action::CancelInviteForm),
        ));
    } else {
        els.push(Element::gesture_button(
            ids::CREATE_INVITE_CODE_BTN,
            t::settings_page::CREATE_CODE,
            true,
            Gesture::Admin(Action::OpenInviteForm),
        ));
    }

    // The freshly minted token — revealed copyable only after a mint (`minted_code`
    // survives the reseed, so the button stays until Cancel). The token itself lists
    // as an `invite-code-value` row below (the re-read), so the button only needs to
    // exist + copy from state (its label names the minted code so a human sees it).
    if let Some(code) = state.minted_code.as_deref() {
        els.push(Element::gesture_button(
            ids::ADMIN_USERS_INVITE_CODE_COPY_BTN,
            t::users_page::minted_code(code),
            true,
            Gesture::Admin(Action::CopyMintedCode),
        ));
    }

    // The existing codes — indexed `invite-code-item` anchor + `invite-code-value` +
    // a per-row delete carrying the code's own token (never a positional guess).
    for code in &snapshot.invite_codes {
        // The minted band echoes on the same row, richer text, no new id
        // (`family-safety.md` § App surface → *Age-band surfaces*).
        let band = code
            .age_band
            .as_deref()
            .and_then(fauna_protocol::age::age_band_label)
            .map(|t| format!(" · {}", t.resolve(fauna_i18n::strings::lookup)))
            .unwrap_or_default();
        els.push(Element::label(
            ids::INVITE_CODE_ITEM,
            format!("{} · {}{band}", code.tier, code.uses_left),
        ));
        els.push(Element::label(ids::INVITE_CODE_VALUE, code.code.clone()));
        els.push(Element::gesture_button(
            ids::ADMIN_SETTINGS_INVITE_DELETE_BUTTON,
            t::users_page::DELETE,
            true,
            Gesture::Admin(Action::DeleteInvite {
                code: code.code.clone(),
            }),
        ));
    }
    els
}

/// A guardian picker (family-safety) — the `none` sentinel plus the handle of
/// each **non-suspended** account on the nest (`UsersSnapshot::picker_users`,
/// every account — never the Users page on screen, `admin.md` § 2 → *Which
/// accounts a picker offers*; the nest re-validates the choice, `admin.rs`
/// guardian rules).
///
/// **A HANDLE picker, not a raw-value one** (`admin.md` § 2 → *What identifies a
/// user in an admin picker*): `actions/admin.py` drives BOTH admission surfaces
/// with a user's **handle** (`create_invite_code`'s `guardian=` →
/// `select("admin-users-invite-guardian-select", handle)`, `set_request_guardian` →
/// `select("invite-request-row-guardian-select", handle)`), so the option list
/// carries handles and `super::resolve_guardian` maps the committed option text
/// back to the wire actor id at mint/approve time — injective because handles are
/// unique (linux's `GuardianSelect` and android's `GuardianDropdown` bind the
/// selection to the actor id directly, the other conformant shape). It IS
/// e2e-driven on tui (`test_family.py`).
fn guardian_picker(
    id: &str,
    draft: &str,
    target: SelectTarget,
    snapshot: &UsersSnapshot,
) -> Element {
    let mut options = vec![GUARDIAN_NONE_VALUE.to_string()];
    options.extend(
        snapshot
            .picker_users
            .iter()
            .filter(|u| !u.suspended)
            .map(super::picker_option),
    );
    Element::select(id, draft.to_string(), target, options).labelled(t::users_page::GUARDIAN_LABEL)
}

/// An age-band picker (`family-safety.md` § App surface → *Age-band surfaces*)
/// — the shared `age_band_options` catalog by VALUE (`not-set` + the four wire
/// tokens, `AgeBand::ORDER`), so no app spells the vocabulary or its order;
/// `actions/admin.py` drives it with the value. **Enabled only while a guardian
/// is selected** (`guardian_selected`): the nest refuses a band without a
/// guardian designation, and the picker gates the same way client-side — a
/// disabled select still paints its (not-set) value, so the driver can read it.
fn age_band_picker(
    id: &str,
    draft: &str,
    target: SelectTarget,
    guardian_selected: bool,
) -> Element {
    let options: Vec<String> = age_band_options().into_iter().map(|o| o.value).collect();
    Element::select(id, draft.to_string(), target, options)
        .labelled(fauna_i18n::strings::family::age_band::LABEL)
        .enabled(guardian_selected)
}

/// Section 4 — the user list (`admin.md` § 2 → Section 4): one row per user with a
/// tier picker (change = quota), the shared-decision lifecycle controls, and the
/// read-only serving audit, above the pagination controls.
fn user_list_section(state: &UsersState, snapshot: &UsersSnapshot) -> Vec<Element> {
    // The `-list-section` view has no terminal chrome, so it paints as a heading
    // label (the `admin-settings-tiers-section` shape).
    let mut els = vec![
        Element::label(ids::ADMIN_USERS_LIST_SECTION, t::users_page::SECTION_USERS),
        Element::label(
            ids::USER_COUNT_TEXT,
            t::users_page::total(&snapshot.total.to_string()),
        ),
    ];

    for (i, user) in snapshot.users.iter().enumerate() {
        // The row anchor shows the display name — the `label`, or the no-handle
        // placeholder for a handle-less account (never the raw actor id, which has
        // its own cell).
        let title = if user.label.is_empty() {
            t::users_page::NO_HANDLE.to_string()
        } else {
            user.label.clone()
        };
        els.push(Element::label(ids::USER_ROW, title));

        // FLAT: the short actor id — the driver's `admin_row_index` prefix-matches
        // the admin's full hex against this truncated display (`short_id` = first 12
        // hex + `…`), so rows are located by identity, never by position.
        els.push(Element::label(
            ids::USER_ACTOR_ID,
            fauna_core::format::short_id(&fauna_core::format::hex_full(user.actor_id.as_ref())),
        ));

        // FLAT: the per-row tier picker (a `Role::Select`, the `_pick_tier` contract).
        // The current tier is both what `get_text` returns and what `select` takes;
        // the row is baked into the target so the flat picker resolves its own user.
        // Selecting applies `users_update` LIVE (the re-read makes `get_text` == target).
        els.push(Element::select(
            ids::ADMIN_USERS_TIER_SELECT,
            user.tier.clone(),
            SelectTarget::UsersTier { row: i },
            snapshot.tiers.clone(),
        ));

        // SCOPED under `user-row[i]`: the read-only IMAP/CalDAV-serving audit — the
        // admin only *sees* it (the user sets it from their own mail-settings; no
        // control here — `admin.md` § Don't do these). Shared bool→key decision so
        // the `serving_here`/`serving_disabled` choice can't drift per client.
        els.push(
            Element::label(
                ids::ADMIN_USERS_MAIL_SERVING_STATUS,
                crate::wizard::localized(&fauna_core::format::mail_serving_status_label(
                    user.mail_serving_enabled,
                )),
            )
            .within(ids::USER_ROW, i),
        );

        // SCOPED under `user-row[i]`: the lifecycle controls the SHARED decision
        // offers — never re-derived from `eviction`/`is_admin` here (`admin.md`
        // § Where logic lives; the rule crosses three eviction states × the admin
        // guard and is easy to get wrong). An admin row offers none of the three.
        let controls = admin_user_row_controls(user);
        if controls.evict {
            els.push(
                Element::gesture_button(
                    ids::ADMIN_USERS_EVICT_BUTTON,
                    t::users_page::EVICT,
                    true,
                    Gesture::Admin(Action::EvictUser { row: i }),
                )
                .within(ids::USER_ROW, i),
            );
        }
        if controls.suspend {
            els.push(
                Element::gesture_button(
                    ids::ADMIN_USERS_SUSPEND_BUTTON,
                    t::users_page::SUSPEND,
                    true,
                    Gesture::Admin(Action::SuspendUser { row: i }),
                )
                .within(ids::USER_ROW, i),
            );
        }
        if controls.restore {
            els.push(
                Element::gesture_button(
                    ids::ADMIN_USERS_CANCEL_EVICTION_BUTTON,
                    t::users_page::CANCEL_EVICTION,
                    true,
                    Gesture::Admin(Action::CancelUserEviction { row: i }),
                )
                .within(ids::USER_ROW, i),
            );
        }
        if controls.make_admin {
            els.push(
                Element::gesture_button(
                    ids::ADMIN_USERS_MAKE_ADMIN_BUTTON,
                    t::users_page::MAKE_ADMIN,
                    true,
                    Gesture::Admin(Action::MakeAdmin { row: i }),
                )
                .within(ids::USER_ROW, i),
            );
        }
        if controls.remove_admin {
            els.push(
                Element::gesture_button(
                    ids::ADMIN_USERS_REMOVE_ADMIN_BUTTON,
                    t::users_page::REMOVE_ADMIN,
                    true,
                    Gesture::Admin(Action::RemoveAdmin { row: i }),
                )
                .within(ids::USER_ROW, i),
            );
        }
    }

    // Pagination — always present (`view` anchor → a page-indicator label; the e2e
    // gates on `count("admin-users-pagination") > 0`). Prev/next are guarded no-ops
    // at the bounds (`apply_local`), so a click at page 1/last leaves the list put.
    let pages = fauna_core::format::total_pages(snapshot.total, USERS_PAGE_SIZE);
    let current = fauna_core::format::current_page(state.offset, USERS_PAGE_SIZE);
    els.push(Element::label(
        ids::ADMIN_USERS_PAGINATION,
        t::users_page::page_indicator(&current.to_string(), &pages.to_string()),
    ));
    els.push(Element::gesture_button(
        ids::ADMIN_USERS_PREV_PAGE,
        t::users_page::PREV_PAGE,
        true,
        Gesture::Admin(Action::UsersPrevPage),
    ));
    els.push(Element::gesture_button(
        ids::ADMIN_USERS_NEXT_PAGE,
        t::users_page::NEXT_PAGE,
        true,
        Gesture::Admin(Action::UsersNextPage),
    ));

    els
}

#[cfg(test)]
mod tests {
    use fauna_client_admin::admin::{
        AdminEviction, AdminInviteCode, AdminInviteRequest, AdminUser,
    };
    use fauna_protocol::ByteBuf;
    use fauna_protocol::admin::AdminPendingActionSummary;

    use super::super::{AdminPage, Outcome, UsersSnapshot, apply_local, apply_outcome};
    use super::*;
    use crate::pages::Page;

    /// Mirrors the nest's admission default (`label` defaults to the handle):
    /// a non-empty name fills BOTH; an empty one is the handle-less account.
    fn user(actor_byte: u8, tier: &str, label: &str) -> AdminUser {
        AdminUser {
            actor_id: ByteBuf::from(vec![actor_byte; 32]),
            tier: tier.to_string(),
            label: label.to_string(),
            handle: (!label.is_empty()).then(|| label.to_string()),
            mail_serving_enabled: true,
            ..Default::default()
        }
    }

    fn request(id: i64, actor_byte: u8, handle: &str, status: &str) -> AdminInviteRequest {
        AdminInviteRequest {
            id,
            actor_id: ByteBuf::from(vec![actor_byte; 32]),
            handle: handle.to_string(),
            message: format!("{handle} would like in"),
            status: status.to_string(),
            ..Default::default()
        }
    }

    fn invite_code(token: &str, tier: &str, uses_left: i64) -> AdminInviteCode {
        AdminInviteCode {
            code: token.to_string(),
            tier: tier.to_string(),
            uses_left,
            ..Default::default()
        }
    }

    fn snapshot(users: Vec<AdminUser>, total: i64) -> UsersSnapshot {
        UsersSnapshot {
            picker_users: users.clone(),
            users,
            total,
            tiers: vec!["free".to_string(), "personal".to_string()],
            invite_codes: vec![],
            invite_requests: vec![],
            registration_mode: Some("closed".to_string()),
            max_free_users: None,
            age_verification_required: false,
            pending_actions: vec![],
        }
    }

    fn pending(
        id: i64,
        action_type: &str,
        target_byte: u8,
        status: &str,
    ) -> AdminPendingActionSummary {
        AdminPendingActionSummary {
            id,
            actor_id: ByteBuf::from(vec![0xaa; 32]),
            action_type: action_type.to_string(),
            target: Some(hex::encode([target_byte; 32])),
            status: status.to_string(),
            created_at: 1,
            execute_after: 86_401,
            requires_quorum: 1,
            approvals: vec![],
            ip_address: None,
            extra: Default::default(),
        }
    }

    fn app_on_users(snapshot: UsersSnapshot) -> crate::app::App {
        let mut app = crate::app::tests::test_app();
        app.admin.sub = AdminPage::Users;
        apply_outcome(&mut app, Outcome::UsersLoaded(Box::new(snapshot)));
        app
    }

    /// Both guardian pickers offer every account on the nest, not the Users page
    /// on screen (`admin.md` § 2 → *Which accounts a picker offers*): an account
    /// the page does not show — here the box claimer, older than every account on
    /// it — is still offered, and picking it resolves to its actor id.
    #[test]
    fn guardian_pickers_offer_accounts_beyond_the_users_page() {
        let mut snap = snapshot(vec![user(0x11, "free", "newest")], 2);
        snap.picker_users = vec![user(0x11, "free", "newest"), user(0x22, "free", "claimer")];
        snap.invite_requests = vec![request(1, 0xaa, "wants-in", "pending")];
        let mut app = app_on_users(snap);
        apply_local(&mut app, Action::OpenInviteForm);

        let els = users_elements(&app.admin);
        for id in [
            "admin-users-invite-guardian-select",
            "invite-request-row-guardian-select",
        ] {
            let el = els.iter().find(|e| e.id == id).expect("picker painted");
            let crate::element::Role::Select { options, .. } = &el.role else {
                panic!("{id} must be a Select");
            };
            assert!(
                options.contains(&"claimer".to_string()),
                "{id} must offer the account missing from the Users page; offered {options:?}"
            );
        }

        apply_local(&mut app, Action::SetInviteGuardian("claimer".to_string()));
        let super::super::UsersMutation::CreateInvite { guardian, .. } =
            super::super::create_invite_mutation(&app.admin.users)
        else {
            panic!("expected CreateInvite");
        };
        assert_eq!(
            guardian,
            Some(vec![0x22; 32]),
            "the pick binds the claimer's actor"
        );
    }

    /// The page paints the five sections in ui.yaml order — pending requests (empty
    /// here → just its anchor), registration (a known posture → picker + ceiling +
    /// save), admit (the always-open direct-admission form), invite (form closed →
    /// the open button), then the user list with flat row cells + scoped controls +
    /// pagination. A clean page registers no `admin-users-action-error`.
    #[test]
    fn users_page_paints_all_five_sections() {
        let app = app_on_users(snapshot(
            vec![user(0x11, "free", "alice"), user(0x22, "personal", "bob")],
            2,
        ));
        let els = users_elements(&app.admin);
        let tagged: Vec<&str> = els
            .iter()
            .map(|e| e.id.as_str())
            .filter(|id| !id.is_empty())
            .collect();
        assert_eq!(
            tagged,
            vec![
                "admin-users-heading",
                "admin-nav-back",
                // Section 1 — pending requests (none pending → just the anchor).
                "admin-users-requests-section",
                // Section 2 — registration (known posture → editable controls).
                "admin-users-registration-section",
                "admin-users-registration-mode-select",
                "admin-users-max-free-users-input",
                "admin-users-registration-age-verification-toggle",
                "admin-users-registration-save-button",
                // Section — admit (direct admission; the form is always open).
                "admin-users-admit-section",
                "admin-users-admit-actor-input",
                "admin-users-admit-handle-input",
                "admin-users-admit-tier-select",
                "admin-users-admit-button",
                // Section — invite (form closed → the reveal button).
                "admin-users-invite-section",
                "create-invite-code-btn",
                // Section 4 — the user list.
                "admin-users-list-section",
                "user-count-text",
                // Row 0
                "user-row",
                "user-actor-id",
                "admin-users-tier-select",
                "admin-users-mail-serving-status",
                "admin-users-evict-button",
                "admin-users-suspend-button",
                "admin-users-make-admin-button",
                // Row 1
                "user-row",
                "user-actor-id",
                "admin-users-tier-select",
                "admin-users-mail-serving-status",
                "admin-users-evict-button",
                "admin-users-suspend-button",
                "admin-users-make-admin-button",
                // Pagination
                "admin-users-pagination",
                "admin-users-prev-page",
                "admin-users-next-page",
                // Section 6 — pending admin actions (none pending → the
                // standing anchor with its empty-state line).
                "admin-users-pending-section",
            ],
            "the sections in ui.yaml order; no action-error on a clean page"
        );
        let pending = els
            .iter()
            .find(|e| e.id == "admin-users-pending-section")
            .unwrap();
        assert_eq!(pending.text, "Nothing is pending.");

        // The count reads the unpaginated total; the tier picker shows the current
        // tier and offers the tier names.
        let count = els.iter().find(|e| e.id == "user-count-text").unwrap();
        assert_eq!(count.text, "2 users total");
        let tier0 = els
            .iter()
            .find(|e| e.id == "admin-users-tier-select")
            .unwrap();
        assert_eq!(
            tier0.text, "free",
            "the picker shows the row's current tier"
        );

        // The scoped serving indicator + lifecycle buttons are pathed under one row.
        let serving = els
            .iter()
            .find(|e| e.id == "admin-users-mail-serving-status")
            .unwrap();
        assert_eq!(serving.text, "Serving here");
        assert_eq!(serving.path.len(), 1, "scoped under one user-row");
    }

    /// Section 6 paints one row per still-PENDING action (a cancelled row is
    /// not offered controls it cannot take), each row's children scoped under
    /// its `admin-pending-action-item`, the description the shared sentence
    /// plus who scheduled it, and the two one-click controls carrying the
    /// action's id (`admin.md` § Pending admin actions).
    #[test]
    fn pending_section_paints_pending_rows_with_their_controls() {
        let mut snap = snapshot(vec![user(0x11, "free", "alice")], 1);
        snap.pending_actions = vec![
            pending(7, "admin.add", 0x11, "pending"),
            pending(8, "admin.delete_user", 0x22, "cancelled"),
        ];
        let app = app_on_users(snap);
        let els = users_elements(&app.admin);
        let of = |id: &str| els.iter().filter(|e| e.id == id).collect::<Vec<_>>();

        let section = of("admin-users-pending-section");
        assert_eq!(section[0].text, "Pending admin actions (1)");
        assert_eq!(
            of("admin-pending-action-item").len(),
            1,
            "the cancelled row is not painted"
        );
        let description = of("admin-pending-action-description");
        assert_eq!(
            description[0].text,
            format!(
                "Grant {} the admin role by aaaaaaaaaaaa…",
                hex::encode([0x11u8; 32])
            )
        );
        assert_eq!(
            description[0].path,
            vec![("admin-pending-action-item".to_string(), 0)]
        );
        assert_eq!(
            of("admin-pending-action-approvals")[0].text,
            "0 of 1 approvals"
        );
        assert!(
            of("admin-pending-action-execute-after")[0]
                .text
                .starts_with("Applies ")
        );
        assert!(matches!(
            of("admin-pending-action-approve-button")[0].role,
            crate::element::Role::Button(Gesture::Admin(Action::ApprovePending { id: 7 }))
        ));
        assert!(matches!(
            of("admin-pending-action-cancel-button")[0].role,
            crate::element::Role::Button(Gesture::Admin(Action::CancelPending { id: 7 }))
        ));
    }

    /// The row's short actor id is `short_id(hex(actor))` — the truncated display the
    /// driver prefix-matches the admin's full hex against.
    #[test]
    fn user_actor_id_is_short_hex_prefix() {
        let app = app_on_users(snapshot(vec![user(0xab, "free", "alice")], 1));
        let els = users_elements(&app.admin);
        let actor = els.iter().find(|e| e.id == "user-actor-id").unwrap();
        // 32 bytes of 0xab → "abab…" — the first 12 hex chars + the `…` ellipsis.
        assert_eq!(actor.text, "abababababab…");
        assert!(actor.text.starts_with("abababababab"));
    }

    /// The shared `admin_user_row_controls` decides which lifecycle/role controls a
    /// row offers — an admin row offers NO cut-off control (it can't be
    /// suspended/evicted) but DOES offer remove-admin; a suspended row offers
    /// restore AND make-admin (granting the role to a suspended user is
    /// deliberate). Rendered from the decision, never re-derived.
    #[test]
    fn row_controls_follow_the_shared_decision() {
        let mut admin_user = user(0x01, "free", "root");
        admin_user.is_admin = true;
        let mut suspended = user(0x02, "free", "banned");
        suspended.eviction = Some(AdminEviction {
            status: "suspended".to_string(),
            reason: String::new(),
            category: String::new(),
            warned_at: None,
            suspend_at: None,
            delete_at: None,
            extra: Default::default(),
        });
        let app = app_on_users(snapshot(vec![admin_user, suspended], 2));
        let els = users_elements(&app.admin);

        // The admin row (row 0) offers no cut-off control, but does offer remove-admin.
        let admin_scoped: Vec<&str> = els
            .iter()
            .filter(|e| e.path.first().map(|p| p.1) == Some(0) && !e.id.is_empty())
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(
            admin_scoped,
            vec![
                "admin-users-mail-serving-status",
                "admin-users-remove-admin-button"
            ],
            "admin row: serving audit + remove-admin only, no evict/suspend/cancel/make-admin"
        );

        // The suspended row (row 1) offers restore and make-admin.
        let suspended_buttons: Vec<&str> = els
            .iter()
            .filter(|e| {
                e.path.first().map(|p| p.1) == Some(1)
                    && e.id.starts_with("admin-users-")
                    && e.id.ends_with("button")
            })
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(
            suspended_buttons,
            vec![
                "admin-users-cancel-eviction-button",
                "admin-users-make-admin-button"
            ],
            "suspended row: restore + make-admin, no remove-admin"
        );
    }

    /// The tier-change resolver preserves the row's label; the pagination guard walks
    /// the offset and stops at the bounds. (`apply_local`'s network wrapping needs a
    /// live client, so the pure resolvers are unit-tested here — the `settings.rs`
    /// shape — and the dispatch is e2e-tested.)
    #[test]
    fn tier_change_and_pagination_resolve() {
        let mut app = app_on_users(snapshot(
            vec![user(0x11, "free", "alice"), user(0x22, "free", "bob")],
            120,
        ));

        // A tier change resolves the row's actor + preserves its (unchanged) label.
        let m =
            super::super::user_change_tier_mutation(&app.admin.users, 1, "personal".to_string())
                .expect("row 1 resolves");
        let super::super::UsersMutation::ChangeTier { actor, tier, label } = m else {
            panic!("expected a ChangeTier mutation");
        };
        assert_eq!(actor, vec![0x22; 32]);
        assert_eq!(tier, "personal");
        assert_eq!(label, "bob", "the unchanged label rides through");

        // Pagination: `apply_local` moves the offset, guarded at both bounds (total
        // 120 → three pages: [0,50), [50,100), [100,120)).
        assert_eq!(app.admin.users.offset, 0);
        apply_local(&mut app, Action::UsersNextPage);
        assert_eq!(app.admin.users.offset, 50);
        apply_local(&mut app, Action::UsersNextPage);
        assert_eq!(app.admin.users.offset, 100);
        apply_local(&mut app, Action::UsersNextPage); // last page (100 + 50 >= 120)
        assert_eq!(
            app.admin.users.offset, 100,
            "next at the last page is a no-op"
        );
        apply_local(&mut app, Action::UsersPrevPage);
        assert_eq!(app.admin.users.offset, 50);
        apply_local(&mut app, Action::UsersPrevPage);
        assert_eq!(app.admin.users.offset, 0);
        apply_local(&mut app, Action::UsersPrevPage); // page 1
        assert_eq!(app.admin.users.offset, 0, "prev at page 1 is a no-op");
    }

    /// A failed mutation folds onto the page-scoped `admin-users-action-error`, NOT
    /// the app-wide `error-message`; a clean `UsersLoaded` clears it.
    #[test]
    fn action_failure_is_page_scoped() {
        let mut app = app_on_users(snapshot(vec![user(0x11, "free", "alice")], 1));
        apply_outcome(
            &mut app,
            Outcome::UsersActionFailed("evict user: boom".to_string()),
        );
        assert_eq!(
            app.admin.users.action_error.as_deref(),
            Some("evict user: boom")
        );
        assert!(
            !app.errors.contains_key(&Page::Admin),
            "a users mutation error never lands on the app-wide error-message"
        );
        let err = users_elements(&app.admin)
            .into_iter()
            .find(|e| e.id == "admin-users-action-error")
            .expect("action-error painted when set");
        assert_eq!(err.text, "evict user: boom");

        // A clean re-read clears it.
        apply_outcome(
            &mut app,
            Outcome::UsersLoaded(Box::new(snapshot(vec![user(0x11, "free", "alice")], 1))),
        );
        assert!(app.admin.users.action_error.is_none());
    }

    // ── Section 2 — Registration ────────────────────────────────────────────

    /// The registration save resolves the picked mode + parsed ceiling; a blank
    /// ceiling clears the cap; a non-numeric ceiling is a user error (no mutation).
    /// An unknown persisted posture renders read-only (no picker), never coerced.
    #[test]
    fn registration_save_and_unknown_posture() {
        let mut app = app_on_users(snapshot(vec![user(0x11, "free", "a")], 1));
        // Persisted "closed" seeds the draft; pick "invite_required" + a ceiling.
        app.admin.users.registration_mode_draft = "invite_required".to_string();
        app.admin.users.max_free_users_input = "50".to_string();
        let m = super::super::registration_mutation(&app.admin.users).expect("valid");
        let super::super::UsersMutation::SaveRegistration { mode, max_free, .. } = m else {
            panic!("expected SaveRegistration");
        };
        assert_eq!(mode, RegistrationMode::InviteRequired);
        assert_eq!(max_free, Some(50));
        // Blank ceiling → no cap.
        app.admin.users.max_free_users_input = "  ".to_string();
        let super::super::UsersMutation::SaveRegistration { max_free, .. } =
            super::super::registration_mutation(&app.admin.users).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(max_free, None, "blank ceiling clears the cap");
        // Non-numeric ceiling → Err (surfaced on the action error, no dispatch).
        app.admin.users.max_free_users_input = "lots".to_string();
        assert!(super::super::registration_mutation(&app.admin.users).is_err());
        let op = apply_local(&mut app, Action::SaveRegistration);
        assert!(op.is_none(), "a bad ceiling dispatches nothing");
        assert_eq!(
            app.admin.users.action_error.as_deref(),
            Some(t::users_page::MAX_FREE_USERS_HINT)
        );

        // An unrecognized posture renders read-only — the picker is absent, the
        // mode-select id carries the explainer instead.
        let mut app = app_on_users(UsersSnapshot {
            registration_mode: Some("galactic".to_string()),
            ..snapshot(vec![user(0x11, "free", "a")], 1)
        });
        app.admin.sub = AdminPage::Users;
        let els = users_elements(&app.admin);
        assert!(
            !els.iter()
                .any(|e| e.id == "admin-users-registration-save-button"),
            "unknown posture: no save button"
        );
        let mode_el = els
            .iter()
            .find(|e| e.id == "admin-users-registration-mode-select")
            .unwrap();
        assert!(
            mode_el.text.contains("galactic"),
            "the unknown posture is named in the read-only explainer"
        );
    }

    // ── Section 3 — Invite codes ────────────────────────────────────────────

    /// The create form toggles open/closed; a mint builds the create mutation with a
    /// defaulted max-uses; a minted token is revealed copyable and the codes list paints.
    #[test]
    fn invite_form_mint_and_list() {
        let mut snap = snapshot(vec![user(0x11, "free", "a")], 1);
        snap.invite_codes = vec![invite_code("TOKEN-1", "personal", 3)];
        let mut app = app_on_users(snap);

        // Closed form → the reveal button; no confirm.
        let closed: Vec<String> = users_elements(&app.admin)
            .iter()
            .map(|e| e.id.clone())
            .collect();
        assert!(closed.iter().any(|id| id == "create-invite-code-btn"));
        assert!(!closed.iter().any(|id| id == "create-invite-confirm-btn"));
        // The existing code lists with its value + a delete.
        assert!(closed.iter().any(|id| id == "invite-code-item"));
        assert!(closed.iter().any(|id| id == "invite-code-value"));

        // Open the form → the tier/uses/guardian/confirm/cancel controls appear.
        apply_local(&mut app, Action::OpenInviteForm);
        let open: Vec<String> = users_elements(&app.admin)
            .iter()
            .map(|e| e.id.clone())
            .collect();
        for id in [
            "admin-settings-tier-select",
            "admin-settings-max-uses-input",
            "admin-users-invite-guardian-select",
            "create-invite-confirm-btn",
            "admin-settings-invite-cancel-button",
        ] {
            assert!(open.iter().any(|e| e == id), "form-open paints {id}");
        }

        // Confirm builds the mint mutation (empty code ⇒ nest mints; uses defaulted).
        app.admin.users.invite_uses_input = "5".to_string();
        let m = super::super::create_invite_mutation(&app.admin.users);
        let super::super::UsersMutation::CreateInvite { uses, guardian, .. } = m else {
            panic!("expected CreateInvite");
        };
        assert_eq!(uses, 5);
        assert_eq!(guardian, None, "the default guardian is none");

        // A mint reveals the token copyable and re-reads the list.
        apply_outcome(
            &mut app,
            Outcome::InviteMinted {
                code: "FRESH-TOKEN".to_string(),
                snapshot: Box::new({
                    let mut s = snapshot(vec![user(0x11, "free", "a")], 1);
                    s.invite_codes = vec![
                        invite_code("TOKEN-1", "personal", 3),
                        invite_code("FRESH-TOKEN", "free", 1),
                    ];
                    s
                }),
            },
        );
        assert_eq!(app.admin.users.minted_code.as_deref(), Some("FRESH-TOKEN"));
        let after: Vec<String> = users_elements(&app.admin)
            .iter()
            .map(|e| e.id.clone())
            .collect();
        assert!(
            after
                .iter()
                .any(|id| id == "admin-users-invite-code-copy-btn"),
            "the copy button is revealed after a mint"
        );
        // Cancel clears the minted token + closes the form.
        apply_local(&mut app, Action::CancelInviteForm);
        assert!(app.admin.users.minted_code.is_none());
        assert!(!app.admin.users.invite_form_open);
    }

    // ── Section 1 — Pending requests ────────────────────────────────────────

    /// The requests section paints one row per PENDING request (a decided one is
    /// skipped); approve/deny resolve the row's id + drafted tier/reason.
    #[test]
    fn requests_paint_pending_and_resolve_approve_deny() {
        let mut snap = snapshot(vec![user(0x11, "free", "admin")], 1);
        snap.invite_requests = vec![
            request(1, 0xaa, "wants-in", "pending"),
            request(2, 0xbb, "already-denied", "denied"),
            request(3, 0xcc, "also-wants-in", "pending"),
        ];
        let mut app = app_on_users(snap);

        // Only the two pending requests paint a handle row.
        let els = users_elements(&app.admin);
        let handles: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "invite-request-row-handle")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(
            handles,
            vec!["wants-in", "also-wants-in"],
            "the decided request is not rendered"
        );

        // The per-request tier draft was seeded (default tier); pick one, then approve.
        apply_local(
            &mut app,
            Action::SetRequestTier {
                row: 0,
                tier: "personal".to_string(),
            },
        );
        let m = super::super::approve_request_mutation(&app.admin.users, 0).expect("row 0");
        let super::super::UsersMutation::ApproveRequest {
            id, tier, guardian, ..
        } = m
        else {
            panic!("expected ApproveRequest");
        };
        assert_eq!(id, 1, "the first request's id");
        assert_eq!(tier.as_deref(), Some("personal"));
        assert_eq!(guardian, None);

        // Deny row 2 (the third request, full-list index 2) with a reason.
        apply_local(
            &mut app,
            Action::SetRequestGuardian {
                row: 2,
                guardian: super::super::GUARDIAN_NONE_VALUE.to_string(),
            },
        );
        super::super::set_field(
            &mut app.admin,
            AdminField::RequestDenyReason { row: 2 },
            "spam".to_string(),
        );
        let m = super::super::deny_request_mutation(&app.admin.users, 2).expect("row 2");
        let super::super::UsersMutation::DenyRequest { id, reason } = m else {
            panic!("expected DenyRequest");
        };
        assert_eq!(id, 3);
        assert_eq!(reason.as_deref(), Some("spam"));
    }

    // ── The age band (family-safety.md § App surface → *Age-band surfaces*) ──

    /// The request row paints the applicant's claim (total — "No app age
    /// verification" when none) and seeds its band select from a nameable
    /// claim; both band selects are disabled until a guardian is chosen and
    /// reset when the guardian is cleared; the mutations carry the band only
    /// beside a guardian.
    #[test]
    fn age_band_pickers_follow_the_guardian_and_the_claim() {
        let mut snap = snapshot(
            vec![user(0x11, "free", "admin"), user(0x22, "free", "mum")],
            2,
        );
        let mut claimed = request(1, 0xaa, "teen", "pending");
        claimed.age_band = Some("13-15".to_string());
        claimed.age_band_provenance = Some("none".to_string());
        let mut odd = request(2, 0xbb, "future", "pending");
        odd.age_band = Some("teen".to_string()); // a newer nest's token — not nameable
        snap.invite_requests = vec![claimed, odd, request(3, 0xcc, "quiet", "pending")];
        let mut app = app_on_users(snap);
        app.admin.users.invite_form_open = true;

        let els = users_elements(&app.admin);
        let claims: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "invite-request-row-age-claim")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(claims.len(), 3, "one claim line per pending row");
        assert!(
            claims[0].contains("13–15") && claims[0].contains("declared"),
            "{}",
            claims[0]
        );
        assert_eq!(
            claims[1], "No app age verification",
            "an unnameable claim reads as none"
        );
        assert_eq!(claims[2], "No app age verification");
        assert_eq!(
            app.admin.users.request_age_band_drafts,
            vec!["13-15", "not-set", "not-set"],
            "the select seeds from a nameable claim only"
        );
        let pickers: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "invite-request-row-age-band-select")
            .collect();
        assert_eq!(pickers.len(), 3);
        assert_eq!(pickers[0].text, "13-15");
        assert!(
            !pickers[0].enabled,
            "no guardian yet → the band select is disabled"
        );
        let invite = els
            .iter()
            .find(|e| e.id == "admin-users-invite-age-band-select")
            .expect("the open invite form paints the band select");
        assert_eq!(invite.text, "not-set");
        assert!(!invite.enabled);

        // Choosing a guardian enables the pickers; the mutations carry the band.
        apply_local(&mut app, Action::SetInviteGuardian("mum".to_string()));
        apply_local(&mut app, Action::SetInviteAgeBand("u13".to_string()));
        apply_local(
            &mut app,
            Action::SetRequestGuardian {
                row: 0,
                guardian: "mum".to_string(),
            },
        );
        let els = users_elements(&app.admin);
        assert!(
            els.iter()
                .find(|e| e.id == "admin-users-invite-age-band-select")
                .unwrap()
                .enabled
        );
        assert!(
            els.iter()
                .find(|e| e.id == "invite-request-row-age-band-select")
                .unwrap()
                .enabled
        );
        let super::super::UsersMutation::CreateInvite {
            guardian, age_band, ..
        } = super::super::create_invite_mutation(&app.admin.users)
        else {
            panic!("expected CreateInvite");
        };
        assert_eq!(guardian, Some(vec![0x22u8; 32]));
        assert_eq!(age_band, Some(fauna_protocol::age::AgeBand::U13));
        let super::super::UsersMutation::ApproveRequest {
            guardian, age_band, ..
        } = super::super::approve_request_mutation(&app.admin.users, 0).expect("row 0")
        else {
            panic!("expected ApproveRequest");
        };
        assert_eq!(guardian, Some(vec![0x22u8; 32]));
        assert_eq!(
            age_band,
            Some(fauna_protocol::age::AgeBand::Teen13To15),
            "the claim-seeded band rides the approve"
        );

        // Clearing the guardian resets the band and drops it from the mutation.
        apply_local(
            &mut app,
            Action::SetInviteGuardian(GUARDIAN_NONE_VALUE.to_string()),
        );
        assert_eq!(app.admin.users.invite_age_band_draft, "not-set");
        let super::super::UsersMutation::CreateInvite { age_band, .. } =
            super::super::create_invite_mutation(&app.admin.users)
        else {
            unreachable!()
        };
        assert_eq!(age_band, None);
        apply_local(
            &mut app,
            Action::SetRequestGuardian {
                row: 0,
                guardian: GUARDIAN_NONE_VALUE.to_string(),
            },
        );
        assert_eq!(app.admin.users.request_age_band_drafts[0], "not-set");

        // A minted code's band echoes on its row.
        let mut app = app_on_users(UsersSnapshot {
            invite_codes: vec![
                {
                    let mut c = invite_code("BAND", "free", 1);
                    c.age_band = Some("16-17".to_string());
                    c
                },
                invite_code("PLAIN", "free", 1),
            ],
            ..snapshot(vec![user(0x11, "free", "admin")], 1)
        });
        app.admin.sub = AdminPage::Users;
        let els = users_elements(&app.admin);
        let rows: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "invite-code-item")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(rows, vec!["free · 1 · 16–17", "free · 1"]);
    }

    /// The require-knob is a draft in the section's one-gesture save: it seeds
    /// from the snapshot, the toggle flips it, and the save carries it ONLY
    /// when it changed.
    #[test]
    fn age_verification_toggle_rides_the_section_save_only_when_changed() {
        let mut app = app_on_users(snapshot(vec![user(0x11, "free", "a")], 1));
        assert!(
            !app.admin.users.age_verification_draft,
            "seeded from the snapshot (off)"
        );
        let toggle = users_elements(&app.admin)
            .into_iter()
            .find(|e| e.id == "admin-users-registration-age-verification-toggle")
            .expect("the toggle paints in the registration section");
        assert_eq!(toggle.text, t::users_page::AGE_VERIFICATION_REQUIRED_LABEL);
        assert_eq!(
            toggle
                .attrs
                .iter()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.as_str()),
            Some("off")
        );
        // Unchanged → the save sends no knob.
        app.admin.users.registration_mode_draft = "closed".to_string();
        let super::super::UsersMutation::SaveRegistration {
            age_verification, ..
        } = super::super::registration_mutation(&app.admin.users).expect("valid")
        else {
            panic!("expected SaveRegistration");
        };
        assert_eq!(age_verification, None);
        // Flipped → the save carries it.
        apply_local(&mut app, Action::SetAgeVerificationRequired(true));
        assert!(app.admin.users.age_verification_draft);
        let super::super::UsersMutation::SaveRegistration {
            age_verification, ..
        } = super::super::registration_mutation(&app.admin.users).expect("valid")
        else {
            panic!("expected SaveRegistration");
        };
        assert_eq!(age_verification, Some(true));
        // A snapshot that already says "on" seeds the draft on, and the
        // toggle's mirror attr follows.
        let app = app_on_users(UsersSnapshot {
            age_verification_required: true,
            ..snapshot(vec![user(0x11, "free", "a")], 1)
        });
        assert!(app.admin.users.age_verification_draft);
    }

    // ── Guardian pickers (family-safety) ───────────────────────────────────

    /// Both admission surfaces are **LABEL** pickers: the options are the "None"
    /// sentinel plus every non-suspended user's handle (a handle-less account
    /// falls back to its actor hex — never an empty, colliding option), the
    /// selected label is what `get_text` returns, and the committed label
    /// resolves back to that user's actor id at mint/approve time.
    ///
    /// This is the contract `actions/admin.py` drives on every app
    /// (`select("admin-users-invite-guardian-select", handle)` /
    /// `select("invite-request-row-guardian-select", handle)`). An actor-hex
    /// option list never matched the handle the driver sent, so the draft stayed
    /// on the sentinel and every ward was admitted UNSUPERVISED — a green select
    /// that did nothing (`test_family.py`).
    #[test]
    fn guardian_pickers_round_trip_labels() {
        let mut users = vec![
            user(0x11, "free", "alice"),
            user(0x22, "free", "bob"),
            user(0x33, "free", ""), // handle-less → its actor hex
            user(0x44, "free", "banned"),
        ];
        users[3].suspended = true;
        let mut snap = snapshot(users, 4);
        snap.invite_requests = vec![request(1, 0xaa, "wants-in", "pending")];
        let mut app = app_on_users(snap);
        apply_local(&mut app, Action::OpenInviteForm);

        let els = users_elements(&app.admin);
        let options = |id: &str| -> Vec<String> {
            let el = els.iter().find(|e| e.id == id).expect("picker painted");
            match &el.role {
                crate::element::Role::Select { options, .. } => options.clone(),
                _ => panic!("{id} must be a Select"),
            }
        };
        let expected = vec![
            "None".to_string(),
            "alice".to_string(),
            "bob".to_string(),
            "33".repeat(32),
        ];
        assert_eq!(
            options("admin-users-invite-guardian-select"),
            expected,
            "the mint picker offers handles, not actor hex; the suspended user is not offered"
        );
        assert_eq!(
            options("invite-request-row-guardian-select"),
            expected,
            "the pending-request picker offers the same option set"
        );

        // The default selection is the sentinel, and it means no guardian.
        fn selected(app: &crate::app::App, id: &str) -> String {
            users_elements(&app.admin)
                .into_iter()
                .find(|e| e.id == id)
                .expect("picker painted")
                .text
        }
        assert_eq!(selected(&app, "admin-users-invite-guardian-select"), "None");
        let mint_guardian = |app: &crate::app::App| {
            let super::super::UsersMutation::CreateInvite { guardian, .. } =
                super::super::create_invite_mutation(&app.admin.users)
            else {
                panic!("expected CreateInvite");
            };
            guardian
        };
        assert_eq!(mint_guardian(&app), None, "the sentinel means no guardian");

        // Selecting a handle round-trips as the picker's text AND resolves to
        // that user's actor id on the mint call.
        apply_local(&mut app, Action::SetInviteGuardian("bob".to_string()));
        assert_eq!(
            selected(&app, "admin-users-invite-guardian-select"),
            "bob",
            "get_text returns the selected LABEL"
        );
        assert_eq!(
            mint_guardian(&app),
            Some(vec![0x22; 32]),
            "the committed label resolves to bob's actor id"
        );

        // The handle-less account is selectable by its hex option.
        apply_local(&mut app, Action::SetInviteGuardian("33".repeat(32)));
        assert_eq!(mint_guardian(&app), Some(vec![0x33; 32]));

        // A label no listed user carries — or a suspended user's, which the
        // picker never offered — resolves to no guardian, never a wrong actor.
        for draft in ["ghost", "banned", "", GUARDIAN_NONE_VALUE] {
            apply_local(&mut app, Action::SetInviteGuardian(draft.to_string()));
            assert_eq!(
                mint_guardian(&app),
                None,
                "{draft:?} must not resolve to an actor"
            );
        }

        // The per-request picker resolves the same way, into the approve call.
        apply_local(
            &mut app,
            Action::SetRequestGuardian {
                row: 0,
                guardian: "alice".to_string(),
            },
        );
        let super::super::UsersMutation::ApproveRequest { guardian, .. } =
            super::super::approve_request_mutation(&app.admin.users, 0).expect("row 0")
        else {
            panic!("expected ApproveRequest");
        };
        assert_eq!(
            guardian,
            Some(vec![0x11; 32]),
            "approving with a guardian label threads alice's actor id"
        );
    }

    /// Guardian-picker leg of the label-collision family (verify-back residue): two non-suspended users share a display LABEL, and `guardian_picker`
    /// must still render two DISTINCT options — its identity is the HANDLE
    /// (`admin.md` § 2), unique on the nest by construction, never the freely
    /// editable label. `guardian_pickers_round_trip_labels` above never covers
    /// this: its fixture users all have distinct labels. Runs through the real
    /// build site (`guardian_picker` via `users_elements`), no RPC fake needed —
    /// the picker options are folded straight from an in-memory `UsersSnapshot`.
    #[test]
    fn guardian_picker_stays_injective_when_labels_collide() {
        let alex = |actor_byte: u8, handle: &str| {
            let mut u = user(actor_byte, "free", handle);
            u.label = "Alex".to_string();
            u
        };
        let mut app = app_on_users(snapshot(vec![alex(1, "alex"), alex(2, "alex2")], 2));
        apply_local(&mut app, Action::OpenInviteForm);
        let els = users_elements(&app.admin);
        let el = els
            .iter()
            .find(|e| e.id == "admin-users-invite-guardian-select")
            .expect("picker painted");
        let crate::element::Role::Select { options, .. } = &el.role else {
            panic!("admin-users-invite-guardian-select must be a Select");
        };
        assert_eq!(
            *options,
            vec!["None".to_string(), "alex".to_string(), "alex2".to_string()],
            "two same-label users must render as two distinct handle options"
        );
    }
}
