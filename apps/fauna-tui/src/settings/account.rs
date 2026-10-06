//! The Settings Account sub-page (split out of `settings/mod.rs`).

use fauna_i18n::strings::{common, settings as t};
use fauna_ui_ids as ids;

use super::{Action, SettingsField, SettingsState};
use crate::element::{Element, Field, Gesture};

/// Settings → Push notifications (`settings.md` § Push notifications; ids
/// signed off 2026-09-26): the section, its one toggle, and the inline failure
/// line — painted only when something failed.
pub(super) fn push_elements(
    state: &SettingsState,
    standing_failure: Option<&'static str>,
) -> Vec<Element> {
    use fauna_i18n::strings::settings::push_notifications as p;
    let mut els = vec![
        Element::label(ids::PUSH_NOTIFICATIONS_SECTION, p::TITLE),
        Element::chrome(p::DEVICE_DESCRIPTION),
        Element::checkbox_gesture(
            ids::PUSH_NOTIFICATIONS_OPT_IN_TOGGLE,
            p::OPT_IN_LABEL,
            state.push.opted_in,
            Gesture::Settings(Action::TogglePushOptIn),
        )
        // The uniform toggle reading every app's witness uses (`get_attr(id,
        // "state")` → "on"/"off", the report-share convention).
        .attr("state", if state.push.opted_in { "on" } else { "off" }),
    ];
    let failure = state
        .push
        .error
        .as_deref()
        .map(|e| format!("{}: {e}", p::UPDATE_FAILED))
        .or_else(|| standing_failure.map(str::to_string));
    if let Some(text) = failure {
        els.push(Element::label(ids::PUSH_NOTIFICATIONS_ERROR, text));
    }
    els
}

/// Build the identity-export QR matrix from the held secret (`settings.md`
/// § Identity export). The payload is the same `(identity, handle)` URI the
/// wizard's import parser accepts, so export → scan → import is closed. `None`
/// when there is no secret (pre-login) or the encode fails.
pub(super) fn build_identity_qr(state: &SettingsState) -> Option<fauna_core::qr_matrix::QrMatrix> {
    if state.secret_hex.is_empty() {
        return None;
    }
    let handle = (!state.handle.is_empty()).then_some(state.handle.as_str());
    let uri = fauna_core::identity_qr::IdentityQr::to_uri(&state.secret_hex, handle);
    fauna_core::qr_matrix::qr_matrix(&uri).ok()
}

/// Render a QR matrix as terminal block art — two module-rows per text line via
/// Unicode half-blocks (the `qrencode -t UTF8` shape), with the `QUIET_ZONE_MODULES`
/// light margin so scanners lock on. Dark modules paint in the foreground; the
/// caller pins an explicit dark-on-light [`Element::colors`] pair (below), so the
/// contrast is guaranteed-scannable in any terminal theme rather than inherited
/// from it (`settings.md` § Identity export — apple hit exactly this: "a
/// theme-inverted QR does not scan").
pub(crate) fn render_qr(matrix: &fauna_core::qr_matrix::QrMatrix) -> String {
    let q = fauna_core::qr_matrix::QUIET_ZONE_MODULES as usize;
    let size = matrix.size as usize;
    let full = size + 2 * q;
    // A module is dark only inside the code proper; the quiet-zone border is light.
    let dark = |x: usize, y: usize| -> bool {
        if x < q || y < q || x >= q + size || y >= q + size {
            return false;
        }
        matrix.modules[(y - q) * size + (x - q)]
    };
    let mut out = String::new();
    let mut y = 0;
    while y < full {
        for x in 0..full {
            let top = dark(x, y);
            let bottom = y + 1 < full && dark(x, y + 1);
            out.push(match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        y += 2;
        if y < full {
            out.push('\n');
        }
    }
    out
}

/// The multi-account switcher block (`long-term-store.md` § Multi-account
/// evolution): the `account-switcher-list` heading, one `account-switcher-item`
/// row per identity (a handle probe + the Stage-2 require-confirm toggle + either
/// the active indicator or a switch/remove pair), and — while a flagged switch is
/// pending — the in-app re-auth prompt. Rendered first on the Account page,
/// mirroring linux's group order. tui reads the registry into a snapshot at the
/// nav edge ([`super::refresh_accounts`]) rather than per frame, so this paint is
/// a dumb walk of `state.accounts`.
fn account_switcher_elements(state: &SettingsState) -> Vec<Element> {
    let mut els = vec![Element::label(
        ids::ACCOUNT_SWITCHER_LIST,
        t::account_page::ACCOUNTS,
    )];
    for (i, row) in state.accounts.iter().enumerate() {
        // The row anchor. A non-active row is the switch target (tap → the
        // Stage-2 gate, never straight to the seam); the active row is a plain
        // label (no self-switch). Both carry `account-switcher-item`, so the
        // `count(...)` and the `[i]` child scope stay aligned with the visible
        // rows.
        els.push(if row.is_active {
            Element::label(ids::ACCOUNT_SWITCHER_ITEM, row.label.clone())
        } else {
            Element::gesture_button(
                ids::ACCOUNT_SWITCHER_ITEM,
                row.label.clone(),
                true,
                Gesture::Settings(Action::SwitchAccount(row.actor_id.clone())),
            )
        });
        // A queryable handle probe scoped to the row — the value the e2e reads.
        // The row anchor's own label already shows it, so this is a probe, not a
        // shim standing in for missing UI.
        els.push(
            Element::label(ids::ACCOUNT_ITEM_HANDLE, row.label.clone())
                .within(ids::ACCOUNT_SWITCHER_ITEM, i),
        );
        // Stage-2 "require confirmation to switch" — on EVERY row incl. the active
        // one: the natural target is the user's admin identity, usually the
        // account you are already on (and the one the admin auto-default flags).
        // Setting it never prompts; only *activating* a flagged account does.
        els.push(
            Element::checkbox_gesture(
                ids::ACCOUNT_REQUIRE_CONFIRM_TOGGLE,
                t::account_page::REQUIRE_CONFIRM_TOGGLE,
                row.require_confirm,
                Gesture::Settings(Action::ToggleRequireConfirm(row.actor_id.clone())),
            )
            .attr("state", if row.require_confirm { "on" } else { "off" })
            .within(ids::ACCOUNT_SWITCHER_ITEM, i),
        );
        if row.is_active {
            els.push(
                Element::label(ids::ACCOUNT_ITEM_ACTIVE_INDICATOR, common::ACTIVE)
                    .within(ids::ACCOUNT_SWITCHER_ITEM, i),
            );
        } else {
            // Remove is offered only for non-active accounts (removing the active
            // one would need an immediate re-route — deferred with the
            // add-account follow-on).
            els.push(
                Element::gesture_button(
                    ids::ACCOUNT_REMOVE_BUTTON,
                    common::REMOVE,
                    true,
                    Gesture::Settings(Action::RemoveAccount(row.actor_id.clone())),
                )
                .within(ids::ACCOUNT_SWITCHER_ITEM, i),
            );
        }
        // "Open as new instance" — every row, active included: tui shipped
        // straight onto the coexisting-lock shape (`account_scope`'s
        // `ServingMode::Concurrent`), so there is no exclusive-lock era to lift
        // the restriction from. Copies the launch command rather than spawning — see
        // `Action::OpenNewInstance`'s doc for why.
        els.push(
            Element::gesture_button(
                ids::ACCOUNT_OPEN_NEW_INSTANCE_BUTTON,
                t::account_page::OPEN_NEW_INSTANCE,
                true,
                Gesture::Settings(Action::OpenNewInstance(row.actor_id.clone())),
            )
            .within(ids::ACCOUNT_SWITCHER_ITEM, i),
        );
        // What actually landed on the clipboard, painted only on the row that
        // was just used — OSC 52 is fire-and-forget into a terminal that may
        // ignore it (`admin/dns.rs`/`settings/web.rs` doctrine).
        if let Some((actor, command)) = &state.open_new_instance_copied
            && *actor == row.actor_id
        {
            els.push(
                Element::label(
                    ids::ACCOUNT_OPEN_NEW_INSTANCE_COMMAND,
                    t::account_page::open_new_instance_copied(command, &row.label),
                )
                .within(ids::ACCOUNT_SWITCHER_ITEM, i),
            );
        }
    }
    // "Add account" — append-mode onboarding (`long-term-store.md` § Multi-account
    // evolution): run the wizard over the live session to onboard/import a NEW
    // identity, then switch to it. A page-level button (NOT a per-row control), so
    // it is deliberately not `within`-scoped; `App::enter_add_account` routes the
    // screen to the wizard while the session stays live underneath.
    els.push(Element::gesture_button(
        ids::ACCOUNT_ADD_BUTTON,
        t::account_page::ADD_ACCOUNT,
        true,
        Gesture::Settings(Action::AddAccount),
    ));
    // The Stage-2 in-app re-auth prompt — shown only while a flagged switch is
    // pending. This is the shape linux/web/tui adopt where the OS offers no native
    // re-auth prompt (`long-term-store.md` § Per-account re-auth): a confirmation,
    // not a credential check. The body names the target account; cancel/Escape is
    // a pure no-op (`Action::ReauthCancel`).
    if let Some(prompt) = &state.reauth_pending {
        els.push(Element::label(
            ids::ACCOUNT_ACTIVATE_REAUTH_PROMPT,
            t::account_page::reauth_prompt_body(&prompt.label),
        ));
        els.push(Element::gesture_button(
            ids::ACCOUNT_ACTIVATE_REAUTH_CONFIRM_BUTTON,
            t::account_page::REAUTH_CONFIRM,
            true,
            Gesture::Settings(Action::ReauthConfirm),
        ));
        els.push(Element::gesture_button(
            ids::ACCOUNT_ACTIVATE_REAUTH_CANCEL_BUTTON,
            common::CANCEL,
            true,
            Gesture::Settings(Action::ReauthCancel),
        ));
    }
    els
}

/// The Account sub-page (`settings.md` § Layout & flow — the pure-actions
/// surface): the multi-account switcher, identity export (placed before handle,
/// the shipped order), the credential-store section (sealed arm only), the
/// full-account data export, handle change, sign out (inline destructive
/// confirm), delete account, and `settings-nav-back`. The page's
/// `error-message` is registered globally by
/// [`crate::ui::register_frame`] (the `tui-settings`/`logs` precedent), so it
/// is not painted here.
pub(super) fn account_elements(
    state: &SettingsState,
    sweep: Option<&crate::settings::SweepStatus>,
    aftermath: &crate::app::AftermathProgress,
    sealed_store_active: bool,
    push_standing_failure: Option<&'static str>,
    review_pass: super::recovery::MemberReviewPass<'_>,
) -> Vec<Element> {
    let mut els = vec![Element::label(ids::PAGE_HEADING, t::account_page::TITLE)];
    // ── Accounts switcher (multi-account) — first, mirroring linux's order ──
    els.extend(account_switcher_elements(state));
    // ── Identity export (settings.md § Identity export) ──
    els.push(Element::label(
        ids::IDENTITY_EXPORT_SECTION,
        t::identity_export::TITLE,
    ));
    els.push(Element::label(
        ids::IDENTITY_EXPORT_DESCRIPTION,
        t::identity_export::DESC,
    ));
    // One toggle; the label flips show ↔ hide. The warning + QR render ONLY while
    // the QR is shown (`identity_qr` is `Some`).
    let showing = state.identity_qr.is_some();
    els.push(Element::gesture_button(
        ids::IDENTITY_EXPORT_SHOW_QR_BUTTON,
        if showing {
            t::identity_export::HIDE_QR
        } else {
            t::identity_export::SHOW_QR
        },
        true,
        Gesture::Settings(Action::ToggleIdentityQr),
    ));
    if let Some(matrix) = &state.identity_qr {
        els.push(Element::label(
            ids::IDENTITY_EXPORT_WARNING,
            t::identity_export::WARNING,
        ));
        // The rendered grid — `get_text` reads the block art; the paint walks the
        // same multi-line string. Visible only while shown. Pinned dark-on-light
        // (pure black on white) regardless of the terminal theme — a QR's module
        // contrast is spec, and an inverted rendering does not scan
        // (`settings.md` § Identity export). Deliberately NOT `Element::art`:
        // that carries a colour pair PER CELL (an image), which would both be
        // wrong for a monochrome QR (one pair, not one per cell) and destroy
        // this element's `get_text`-scannable 4-glyph text.
        els.push(
            Element::label(ids::IDENTITY_EXPORT_QR, render_qr(matrix))
                .colors([0, 0, 0], [255, 255, 255]),
        );
    }
    // ── Recovery kit (settings.md § Recovery kit) ──
    // Placed immediately after Identity export, the ratified position: both
    // reveal a root secret once, as 64-hex + QR, and neither persists anything.
    els.extend(super::recovery::recovery_elements(
        state,
        sweep,
        aftermath,
        review_pass,
    ));
    // ── Credential store (settings.md § Credential store) — the third
    //    root-secret-custody affordance, after Recovery kit. Sealed arm only:
    //    the OS-store arms have no passphrase to change, so the whole section
    //    is absent there (never painted-but-inert). ──
    if sealed_store_active {
        els.extend(credential_store_elements(state));
    }
    // ── Push notifications (settings.md § Push notifications) — item 10, so
    //    directly before Data export (item 11). One toggle: this install's
    //    opt-in bit, never the OS permission. The inline line carries the
    //    last toggle's failure, else the desktops' standing causes (the agent
    //    unreachable, no notification sink) while opted in. ──
    els.extend(push_elements(state, push_standing_failure));
    // ── Data export (settings.md § Data export; `account-data-plane.md`
    //    *Payload stores* decision (5) — the full archive is the only shape,
    //    no toggle). The wire half is one line — fetch EXPORT_FULL, never
    //    compose the URL — and tui has no save dialog to raise, so the
    //    archive lands in the downloads dir like a snapshot download. ──
    els.push(Element::chrome(t::account_page::EXPORT_SUBTITLE));
    els.push(Element::gesture_button(
        ids::SETTINGS_EXPORT_DATA_BUTTON,
        t::account_page::EXPORT_MY_DATA,
        true,
        Gesture::Settings(Action::ExportData),
    ));
    // ── Handle change (format-validated pre-submit; conflict is server-side) ──
    els.push(
        Element::input(
            ids::NEW_HANDLE,
            state.handle_input.clone(),
            Field::Settings(SettingsField::NewHandle),
        )
        .labelled(t::account_page::NEW_HANDLE),
    );
    els.push(Element::gesture_button(
        ids::CHANGE_HANDLE,
        t::account_page::CHANGE_HANDLE,
        true,
        Gesture::Settings(Action::ChangeHandle),
    ));
    // ── Sign out (arm → inline confirm → wipe) ──
    els.push(Element::gesture_button(
        ids::SIGN_OUT_BUTTON,
        t::SIGN_OUT,
        true,
        Gesture::Settings(Action::SignOut),
    ));
    if state.sign_out_pending {
        els.push(Element::gesture_button(
            ids::SIGN_OUT_CONFIRM_BUTTON,
            t::SIGN_OUT,
            true,
            Gesture::Settings(Action::SignOutConfirm),
        ));
    }
    // ── Delete account (type-to-confirm gate, mirroring web/windows) ──
    els.push(
        Element::input(
            ids::SETTINGS_DELETE_CONFIRM_FIELD,
            state.delete_confirm_input.clone(),
            Field::Settings(SettingsField::DeleteConfirmInput),
        )
        .labelled(t::DELETE_CONFIRM_PLACEHOLDER),
    );
    els.push(Element::gesture_button(
        ids::SETTINGS_DELETE_ACCOUNT_BUTTON,
        t::account_page::DELETE_ACCOUNT,
        state.delete_confirm_input == "DELETE",
        Gesture::Settings(Action::DeleteAccount),
    ));
    els.extend(pending_actions_elements(state));
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

/// The STANDING pending-actions section (`settings.md` § Pending actions,
/// ui.yaml's five `pending-action*` ids): always present — a conditional
/// render would hide the affordance exactly when a mis-clicker goes looking —
/// sitting below the two delayed verbs this page hosts (the third,
/// snapshot delete, schedules from the Backups page and appears here on the
/// next Account visit's hydrate). The container's text answers honestly in
/// all three states (the attendee-list shape): bare title while un-hydrated
/// — never a settled "nothing scheduled" claim with no basis (the
/// un-hydrated-paint rule this queue carries) — the empty-state line when
/// loaded and empty, the counted title with rows. Cancel is ONE CLICK, no
/// confirm: cancelling is the safe direction.
fn pending_actions_elements(state: &SettingsState) -> Vec<Element> {
    use fauna_i18n::strings::settings::pending_actions as pa;
    let mut els = Vec::new();
    els.push(Element::label(
        ids::PENDING_ACTIONS_SECTION,
        match &state.pending_actions {
            None => pa::TITLE.to_string(),
            Some(rows) if rows.is_empty() => pa::NONE_SCHEDULED.to_string(),
            Some(rows) => pa::title_count(&rows.len().to_string()),
        },
    ));
    if let Some(rows) = &state.pending_actions {
        for (i, row) in rows.iter().enumerate() {
            // The tiers-row idiom: a bare row marker + leaves, each
            // `.within(row, i)` so both flat `pending-action-description[1]`
            // and `scope="pending-action-item[1]"` queries resolve.
            els.push(
                Element::label(ids::PENDING_ACTION_ITEM, " ").within(ids::PENDING_ACTION_ITEM, i),
            );
            els.push(
                Element::label(ids::PENDING_ACTION_DESCRIPTION, pending_description(row))
                    .within(ids::PENDING_ACTION_ITEM, i),
            );
            els.push(
                Element::label(
                    ids::PENDING_ACTION_EXECUTE_AFTER,
                    pa::applies(&fauna_core::format::format_unix_local(row.execute_after)),
                )
                .within(ids::PENDING_ACTION_ITEM, i),
            );
            els.push(
                Element::gesture_button(
                    ids::PENDING_ACTION_CANCEL_BUTTON,
                    pa::CANCEL,
                    true,
                    Gesture::Settings(Action::PendingActionCancel(row.id)),
                )
                .within(ids::PENDING_ACTION_ITEM, i),
            );
        }
    }
    els
}

/// What will happen, as one sentence — verb + target (`ui.yaml`'s
/// `pending-action-description` contract). `PendingActionRow` is this app's
/// own narrowed projection of the wire `PendingActionSummary`, so this stays
/// a thin delegate rather than the rendering rule itself — see
/// [`fauna_protocol::pending_actions::describe_pending_action`] for that
/// (shared with linux's identical rendering).
pub(super) fn pending_description(row: &super::PendingActionRow) -> String {
    fauna_protocol::pending_actions::describe_pending_action(
        &row.action_type,
        row.target.as_deref(),
    )
}

/// The credential-store section (`settings.md` § Credential store; sealed-arm
/// mechanics `architecture/apps/tui.md` § Credential storage): status line,
/// the change-passphrase trigger, and — while armed — the re-key modal. The
/// caller gates on the sealed arm being active. The three inputs carry the
/// tui-unlock masking contract: the registered text is a same-length bullet
/// mask ([`crate::unlock::mask`]), so the raw values never enter the
/// automation registry or `/app/state`.
fn credential_store_elements(state: &SettingsState) -> Vec<Element> {
    use fauna_i18n::strings::credential_store as cs;

    let mut els = vec![
        Element::label(ids::CREDENTIAL_STORE_SECTION, cs::SECTION_TITLE),
        Element::label(ids::CREDENTIAL_STORE_STATUS, cs::STATUS_SEALED),
        Element::gesture_button(
            ids::CREDENTIAL_STORE_REKEY_BUTTON,
            cs::REKEY_BUTTON,
            true,
            Gesture::Settings(Action::CredentialStoreOpenRekey),
        ),
    ];
    if state.rekey_success {
        els.push(Element::label(
            ids::CREDENTIAL_STORE_REKEY_SUCCESS,
            cs::SUCCESS,
        ));
    }
    if let Some(rekey) = &state.rekey {
        els.push(Element::label(
            ids::CREDENTIAL_STORE_REKEY_MODAL,
            cs::REKEY_TITLE,
        ));
        // The mandated nudge, ABOVE the inputs: the user reads why a backup
        // matters before they choose the new passphrase, not after.
        els.push(Element::label(
            ids::CREDENTIAL_STORE_REKEY_SEED_NUDGE,
            cs::SEED_NUDGE,
        ));
        els.push(
            Element::input(
                ids::CREDENTIAL_STORE_REKEY_CURRENT_INPUT,
                crate::unlock::mask(rekey.current.as_str()),
                Field::Settings(SettingsField::RekeyCurrent),
            )
            .labelled(cs::CURRENT_LABEL),
        );
        els.push(
            Element::input(
                ids::CREDENTIAL_STORE_REKEY_NEW_INPUT,
                crate::unlock::mask(rekey.new_pass.as_str()),
                Field::Settings(SettingsField::RekeyNew),
            )
            .labelled(cs::NEW_LABEL),
        );
        els.push(
            Element::input(
                ids::CREDENTIAL_STORE_REKEY_CONFIRM_INPUT,
                crate::unlock::mask(rekey.confirm.as_str()),
                Field::Settings(SettingsField::RekeyConfirm),
            )
            .labelled(cs::CONFIRM_LABEL),
        );
        els.push(Element::gesture_button(
            ids::CREDENTIAL_STORE_REKEY_SUBMIT_BUTTON,
            cs::SUBMIT,
            true,
            Gesture::Settings(Action::CredentialStoreRekeySubmit),
        ));
        els.push(Element::gesture_button(
            ids::CREDENTIAL_STORE_REKEY_CANCEL_BUTTON,
            common::CANCEL,
            true,
            Gesture::Settings(Action::CredentialStoreRekeyCancel),
        ));
    }
    els
}

/// Run the change-passphrase submit (`settings.md` § Credential store):
/// validate the modal's buffers, then drive
/// [`fauna_credential_store::sealed::SealedFileStore::change_passphrase`]
/// **synchronously** — the unlock-submit precedent (two Argon2id derives sit
/// under the driver's single-shot-read budget, and a spawned derive would let
/// the driver read a pre-re-key frame). Validation and the wrong-passphrase
/// verdict paint the page's `error-message` with the modal kept open; success
/// disarms the modal (the `SecretString` buffers zeroize on the drop) and
/// paints `credential-store-rekey-success`.
pub(super) fn credential_rekey_submit(app: &mut crate::app::App) {
    use crate::pages::Page;
    use fauna_credential_store::sealed::SealedStoreError;
    use fauna_i18n::strings::credential_store as cs;

    let Some(rekey) = app.settings.rekey.as_ref() else {
        // Unreachable: the submit button paints only while the modal is armed.
        return;
    };
    if rekey.current.is_empty() || rekey.new_pass.is_empty() {
        app.errors
            .insert(Page::Settings, cs::ERROR_EMPTY.to_string());
        return;
    }
    if rekey.new_pass.as_str() != rekey.confirm.as_str() {
        app.errors
            .insert(Page::Settings, cs::ERROR_MISMATCH.to_string());
        return;
    }
    let Some(sealed) = app.credentials.sealed_backend() else {
        // Unreachable: the section paints only with the sealed arm active.
        tracing::error!("[settings] re-key submit with no sealed backend");
        return;
    };
    match sealed.change_passphrase(rekey.current.as_str(), rekey.new_pass.as_str()) {
        Ok(()) => {
            app.errors.remove(&Page::Settings);
            app.settings.rekey = None;
            app.settings.rekey_success = true;
        }
        Err(SealedStoreError::WrongPassphraseOrCorrupt) => {
            app.errors
                .insert(Page::Settings, cs::ERROR_WRONG.to_string());
        }
        Err(e) => {
            app.errors
                .insert(Page::Settings, cs::error_failed(&e.to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use fauna_credential_store::CredentialStore;
    use fauna_credential_store::sealed::{SealedFileStore, SealedStoreError};
    use fauna_i18n::strings::credential_store as cs;

    use super::*;
    use crate::app::AftermathProgress;
    use crate::pages::Page;
    use crate::settings::{apply, set_field};

    fn ids(els: &[Element]) -> Vec<String> {
        els.iter().map(|e| e.id.clone()).collect()
    }

    fn account_ids(state: &SettingsState, sealed: bool) -> Vec<String> {
        ids(&account_elements(
            state,
            None,
            &AftermathProgress::default(),
            sealed,
            None,
            crate::settings::recovery::MemberReviewPass::empty(),
        ))
    }

    /// `settings.md` § Pending actions: the section is STANDING (present in
    /// every state) and its container text answers honestly in all three —
    /// bare title while un-hydrated (never a settled "nothing scheduled"
    /// claim with no basis, the un-hydrated-paint rule), the empty-state
    /// line when loaded and empty, the counted title with rows.
    #[test]
    fn the_pending_actions_section_is_standing_and_answers_honestly() {
        use fauna_i18n::strings::settings::pending_actions as pa;
        let section_text = |state: &SettingsState| {
            account_elements(
                state,
                None,
                &AftermathProgress::default(),
                false,
                None,
                crate::settings::recovery::MemberReviewPass::empty(),
            )
            .iter()
            .find(|e| e.id == "pending-actions-section")
            .expect("the section is standing — present in every state")
            .text
            .clone()
        };

        // (a) un-hydrated: no claim about the schedule.
        let mut state = SettingsState::default();
        assert_eq!(section_text(&state), pa::TITLE);

        // (b) loaded, genuinely empty.
        state.pending_actions = Some(Vec::new());
        assert_eq!(section_text(&state), pa::NONE_SCHEDULED);

        // (c) loaded with rows — the counted title.
        state.pending_actions = Some(vec![row("handle.change", Some("bob"), 11)]);
        assert_eq!(section_text(&state), pa::title_count("1"));
    }

    fn row(action_type: &str, target: Option<&str>, id: i64) -> crate::settings::PendingActionRow {
        crate::settings::PendingActionRow {
            id,
            action_type: action_type.to_string(),
            target: target.map(str::to_string),
            execute_after: 1_760_000_000,
        }
    }

    /// Each scheduled row registers the ui.yaml quartet, scoped under its own
    /// `pending-action-item` occurrence (the tiers-row idiom), and the cancel
    /// button carries THAT row's wire id — one click cancels the row it sits
    /// on, never a positional guess re-derived at dispatch time.
    #[test]
    fn a_pending_row_registers_its_children_and_cancel_carries_the_row_id() {
        use fauna_i18n::strings::settings::pending_actions as pa;
        let state = SettingsState {
            pending_actions: Some(vec![
                row("handle.change", Some("bob"), 7),
                row("account.delete", None, 9),
            ]),
            ..SettingsState::default()
        };
        let els = account_elements(
            &state,
            None,
            &AftermathProgress::default(),
            false,
            None,
            crate::settings::recovery::MemberReviewPass::empty(),
        );
        let of = |id: &str| -> Vec<&Element> { els.iter().filter(|e| e.id == id).collect() };
        assert_eq!(of("pending-action-item").len(), 2);
        let descriptions = of("pending-action-description");
        assert_eq!(descriptions[0].text, pa::change_handle_to("bob"));
        assert_eq!(descriptions[1].text, pa::DELETE_ACCOUNT);
        assert_eq!(
            descriptions[1].path,
            vec![("pending-action-item".to_string(), 1)]
        );
        let afters = of("pending-action-execute-after");
        assert_eq!(afters.len(), 2);
        assert!(
            afters[0].text.contains("Applies"),
            "the when-line wears the applies wording: {:?}",
            afters[0].text
        );
        let cancels = of("pending-action-cancel-button");
        assert_eq!(cancels.len(), 2);
        for (cancel, want) in cancels.iter().zip([7i64, 9]) {
            match &cancel.role {
                crate::element::Role::Button(Gesture::Settings(Action::PendingActionCancel(
                    got,
                ))) => {
                    assert_eq!(*got, want, "the cancel carries its own row's wire id");
                }
                other => panic!("cancel must carry PendingActionCancel, got {other:?}"),
            }
        }
    }

    /// `pending_description` is now a thin delegate to
    /// `fauna_protocol::pending_actions::describe_pending_action`, which
    /// carries the full verb/skew coverage
    /// (`describe_pending_action_names_each_verb_and_survives_skew`) — this
    /// just proves the delegate actually wires this app's row fields
    /// through.
    #[test]
    fn pending_description_delegates_to_the_shared_renderer() {
        assert_eq!(
            pending_description(&row("handle.change", Some("bob"), 1)),
            fauna_protocol::pending_actions::describe_pending_action("handle.change", Some("bob"))
        );
    }

    /// `settings.md` § Credential store: the section renders ONLY while the
    /// sealed arm is active — absent entirely otherwise (never
    /// painted-but-inert), and the modal ids wait for the arming gesture.
    #[test]
    fn the_credential_store_section_renders_only_on_the_sealed_arm() {
        let state = SettingsState::default();
        let without = account_ids(&state, false);
        assert!(
            !without.iter().any(|id| id.starts_with("credential-store-")),
            "no credential-store id may render off the sealed arm: {without:?}"
        );
        let with = account_ids(&state, true);
        for id in [
            "credential-store-section",
            "credential-store-status",
            "credential-store-rekey-button",
        ] {
            assert!(with.contains(&id.to_string()), "{id} must render");
        }
        assert!(
            !with.contains(&"credential-store-rekey-modal".to_string()),
            "the modal paints only while armed"
        );
    }

    /// The armed modal paints the ui.yaml family in order, and every input's
    /// registered text is a same-length bullet mask — the raw values must
    /// never reach the registry (the tui-unlock contract).
    #[test]
    fn the_armed_modal_paints_the_ui_yaml_family_and_masks_every_buffer() {
        let mut state = SettingsState::default();
        apply(&mut state, Action::CredentialStoreOpenRekey);
        set_field(&mut state, SettingsField::RekeyCurrent, "hunter2".into());
        set_field(&mut state, SettingsField::RekeyNew, "swordfish".into());
        set_field(&mut state, SettingsField::RekeyConfirm, "sw".into());
        let els = account_elements(
            &state,
            None,
            &AftermathProgress::default(),
            true,
            None,
            crate::settings::recovery::MemberReviewPass::empty(),
        );
        let all = ids(&els);
        let start = all
            .iter()
            .position(|i| i == "credential-store-rekey-modal")
            .expect("the armed modal renders");
        assert_eq!(
            &all[start..start + 7],
            &[
                "credential-store-rekey-modal",
                "credential-store-rekey-seed-nudge",
                "credential-store-rekey-current-input",
                "credential-store-rekey-new-input",
                "credential-store-rekey-confirm-input",
                "credential-store-rekey-submit-button",
                "credential-store-rekey-cancel-button",
            ],
        );
        let text_of = |id: &str| els.iter().find(|e| e.id == id).unwrap().text.clone();
        assert_eq!(text_of("credential-store-rekey-current-input"), "•••••••");
        assert_eq!(text_of("credential-store-rekey-new-input"), "•••••••••");
        assert_eq!(text_of("credential-store-rekey-confirm-input"), "••");
        assert!(
            !els.iter()
                .any(|e| e.text.contains("hunter2") || e.text.contains("swordfish")),
            "a raw passphrase must never enter the registry"
        );
    }

    /// Arming mints a fresh buffer set and clears a prior success line;
    /// cancel disarms with no side effect.
    #[test]
    fn open_clears_the_success_line_and_cancel_disarms() {
        let mut state = SettingsState {
            rekey_success: true,
            ..Default::default()
        };
        apply(&mut state, Action::CredentialStoreOpenRekey);
        assert!(state.rekey.is_some());
        assert!(
            !state.rekey_success,
            "arming clears the stale success claim"
        );
        apply(&mut state, Action::CredentialStoreRekeyCancel);
        assert!(state.rekey.is_none());
    }

    /// Empty / mismatched entries paint the page error and keep the modal
    /// open — nothing reaches the store.
    #[test]
    fn submit_validation_paints_the_page_error_and_keeps_the_modal() {
        let mut app = crate::app::tests::test_app();
        apply(&mut app.settings, Action::CredentialStoreOpenRekey);
        credential_rekey_submit(&mut app);
        assert_eq!(
            app.errors.get(&Page::Settings).map(String::as_str),
            Some(cs::ERROR_EMPTY)
        );
        assert!(app.settings.rekey.is_some(), "the modal stays");
        set_field(&mut app.settings, SettingsField::RekeyCurrent, "old".into());
        set_field(&mut app.settings, SettingsField::RekeyNew, "a".into());
        set_field(&mut app.settings, SettingsField::RekeyConfirm, "b".into());
        credential_rekey_submit(&mut app);
        assert_eq!(
            app.errors.get(&Page::Settings).map(String::as_str),
            Some(cs::ERROR_MISMATCH)
        );
        assert!(app.settings.rekey.is_some(), "the modal stays");
    }

    /// The full submit against a REAL sealed store: a wrong current
    /// passphrase answers the honest error with the store untouched; the
    /// right one disarms the modal, paints success, and leaves a file that
    /// opens ONLY under the new passphrase.
    #[test]
    fn submit_rekeys_the_real_sealed_store() {
        let dir = std::env::temp_dir().join(format!("fauna-tui-rekey-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ns = "fauna-tui-rekey-test";
        let creds = Arc::new(CredentialStore::with_sealed_backend(ns, dir.clone()));
        creds
            .sealed_backend()
            .expect("the sealed twin constructor resolves to the sealed arm")
            .create("old horse")
            .expect("create the store under test");
        let mut app =
            crate::app::App::with_credentials(&tokio::sync::mpsc::unbounded_channel().0, creds);
        apply(&mut app.settings, Action::CredentialStoreOpenRekey);
        set_field(
            &mut app.settings,
            SettingsField::RekeyCurrent,
            "wrong".into(),
        );
        set_field(
            &mut app.settings,
            SettingsField::RekeyNew,
            "new horse".into(),
        );
        set_field(
            &mut app.settings,
            SettingsField::RekeyConfirm,
            "new horse".into(),
        );
        credential_rekey_submit(&mut app);
        assert_eq!(
            app.errors.get(&Page::Settings).map(String::as_str),
            Some(cs::ERROR_WRONG)
        );
        assert!(app.settings.rekey.is_some(), "the modal stays on refusal");
        assert!(!app.settings.rekey_success);

        set_field(
            &mut app.settings,
            SettingsField::RekeyCurrent,
            "old horse".into(),
        );
        credential_rekey_submit(&mut app);
        assert!(!app.errors.contains_key(&Page::Settings), "error cleared");
        assert!(app.settings.rekey.is_none(), "success disarms the modal");
        assert!(app.settings.rekey_success, "the success line paints");

        // The effect, not the call: a fresh store over the same file opens
        // only under the new passphrase.
        let fresh = SealedFileStore::new(ns, dir.clone());
        assert!(matches!(
            fresh.unlock("old horse"),
            Err(SealedStoreError::WrongPassphraseOrCorrupt)
        ));
        fresh.unlock("new horse").expect("the new passphrase opens");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
