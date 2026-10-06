use adw::prelude::*;
use fauna_ui_ids as ids;
use gtk::gio;

use crate::i18n::strings::common;
use crate::i18n::strings::settings::account_page as ap;

thread_local! {
    /// Re-entrancy guard for the Stage-2 confirm prompt. A single real click on an
    /// activatable row fires the per-row `connect_activated` *and* the parent
    /// listbox's `row-activated` (the same double-fire the "Add account" row dodges
    /// by not connecting per-row — see its comment below). The switch handler's own
    /// guard sits *downstream* of this prompt, so without a guard here one click
    /// would stack two dialogs.
    static REAUTH_PROMPT_OPEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The live Account page's `error-message`, for the one writer that runs
    /// outside the page's own closures: the switch handler in `main.rs`, whose
    /// refusal must be painted where the user clicked
    /// ([`paint_switch_refusal`]). Weak — the page owns its label.
    static ACCOUNT_ERROR_LABEL: std::cell::RefCell<Option<gtk::glib::WeakRef<gtk::Label>>> =
        const { std::cell::RefCell::new(None) };
}

/// Paint a switch the registry refused (`set_active` — the device cannot sign
/// in as `actor_id`, or any other refusal) on the Account page's
/// `error-message`, through the shared line
/// (`fauna_client_accounts::switch_refused_copy`): it names the target and says
/// the user is still on the identity they were using, which is true because a
/// refusal at `set_active` precedes every teardown (`long-term-store.md`
/// § Multi-account evolution, "Activating refuses an account it cannot launch
/// as"). A click that did nothing and said nothing would read as a broken
/// button. tui's `settings::paint_switch_refusal`.
pub fn paint_switch_refusal(actor_id: &str, err: &fauna_client_accounts::AccountError) {
    let label = crate::account_registry()
        .list()
        .into_iter()
        .find(|a| a.actor_id == actor_id)
        .map(|a| fauna_core::format::account_display_label(a.handle.as_deref(), &a.actor_id))
        .unwrap_or_else(|| actor_id.to_string());
    let line = fauna_client_accounts::switch_refused_copy(err, &label)
        .resolve(crate::i18n::strings::lookup);
    tracing::error!("[account-switch] refused: {err}");
    match ACCOUNT_ERROR_LABEL.with(|cell| cell.borrow().as_ref().and_then(|w| w.upgrade())) {
        Some(error_label) => super::render_account_error_label(&error_label, Some(&line)),
        None => tracing::warn!("[account-switch] no Account page to paint the refusal on"),
    }
}

/// The Stage-2 gate in front of the app's switch seam (`long-term-store.md`
/// § Multi-account evolution → *Per-account re-auth*). Resolves the re-auth
/// question **before** anything app-owned runs, so the switch handler's
/// mutation-first invariant (registry write before teardown) holds unchanged.
///
/// Linux has **no native OS re-auth prompt** (unlike apple's `LAContext` or
/// windows Hello), so this renders the in-app `account-activate-reauth-prompt`
/// surface — the shape linux ratifies for the no-native-prompt platforms; web and
/// tui adopt it. It is a confirmation, not a credential check: the ratified
/// degradation for platforms the OS gives nothing better on.
///
/// The flag is read **fresh** from the registry rather than off the build-time row
/// entry: this is a build-once page, so a toggle flipped since it was built would
/// otherwise read stale `false`, skip the prompt, and let the registry refuse the
/// activation with `ConfirmationRequired` — fail-closed, but a dead-feeling click.
///
/// **Declining is a pure no-op** (ratified): no registry mutation, no teardown, no
/// error banner — the user stays on the account they were already using.
fn request_switch_account(actor_id: String, from: &gtk::Widget) {
    let registry = crate::account_registry();
    let entry = registry.list().into_iter().find(|a| a.actor_id == actor_id);
    if !entry
        .as_ref()
        .is_some_and(|a| a.require_confirm_to_activate)
    {
        crate::settings::trigger_switch_account(actor_id, false);
        return;
    }
    if REAUTH_PROMPT_OPEN.with(|p| p.replace(true)) {
        return;
    }
    let label = entry
        .as_ref()
        .map(|a| fauna_core::format::account_display_label(a.handle.as_deref(), &a.actor_id))
        .unwrap_or_else(|| actor_id.clone());

    // The dialog IS the ui.yaml `view` (a runtime construct, like the sign-out
    // confirm below). Suggested, not Destructive: this gate stands in front of
    // an account switch, which is reversible — declining is a pure no-op (no
    // registry mutation, no teardown, no error banner).
    crate::confirm_dialog::present_confirm(
        from,
        crate::confirm_dialog::ConfirmSpec::new(
            ap::REAUTH_PROMPT_TITLE,
            crate::confirm_dialog::ConfirmBody::Text(&ap::reauth_prompt_body(&label)),
            "switch",
            ap::REAUTH_CONFIRM,
            common::CANCEL,
        )
        .suggested()
        .with_dialog_id(ids::ACCOUNT_ACTIVATE_REAUTH_PROMPT)
        .with_confirm_id(ids::ACCOUNT_ACTIVATE_REAUTH_CONFIRM_BUTTON)
        .with_cancel_id(ids::ACCOUNT_ACTIVATE_REAUTH_CANCEL_BUTTON)
        // Cleared on EVERY answer, not just the confirm: declining must not
        // leave the guard latched, or one cancel wedges the gesture for the
        // rest of the session.
        .with_dismiss(|| REAUTH_PROMPT_OPEN.with(|p| p.set(false))),
        move || {
            // The user just confirmed → the post-re-auth path. This is the ONLY
            // `confirmed = true` call site: the audit surface for the gate.
            crate::settings::trigger_switch_account(actor_id.clone(), true);
        },
    );
}

/// The sign-out confirm's body, with its refusal passed in — the same shape
/// as tui's `settings::sign_out_confirm_unless`, so
/// a test can reach the gesture without a live sibling on this machine's real
/// config dirs (the question's own answer is pinned in `account_scope` over
/// temp bases).
///
/// The refusal runs BEFORE `trigger_sign_out`, not inside it: that handler
/// erases every account's stores and then wipes the credential namespace —
/// there is no half of it that is safe under a live sibling.
pub(crate) fn sign_out_confirm_unless(
    error_label: &gtk::Label,
    blocked: impl FnOnce() -> Option<String>,
) {
    if let Some(line) = blocked() {
        super::render_account_error_label(error_label, Some(&line));
        return;
    }
    crate::settings::trigger_sign_out();
}

/// The remove-account button's confirm body, with the real removal call
/// passed in — never a stub, so a test drives its actual refusal (including
/// the this-window case, `account-scoping.md:982`) over `account_scope`'s
/// temp-base `Seats` harness, the same shape as [`sign_out_confirm_unless`]. `on_removed` does the page's own listbox
/// surgery, which only the real page can do.
pub(crate) fn remove_account_confirm_unless(
    actor_id: &str,
    error_label: &gtk::Label,
    remove: impl FnOnce(&str) -> Result<(), String>,
    on_removed: impl FnOnce(),
) {
    if let Err(line) = remove(actor_id) {
        super::render_account_error_label(error_label, Some(&line));
        return;
    }
    super::render_account_error_label(error_label, None);
    on_removed();
}

/// Build the "Account" preferences page.
///
/// Returns the page plus an on-visible refresh closure: the page is built once, but
/// the Stage-2 `require_confirm_to_activate` flag can change after the build (the
/// admin auto-default writes it at the am-i-admin observation), so the switcher's
/// toggles must re-read the registry when the page is shown. Wire it to the shell
/// stack's visible-child notify (`views::settings_shell`).
pub fn build_account_page() -> (gtk::Box, std::rc::Rc<dyn Fn()>) {
    let page = adw::PreferencesPage::builder()
        .title(ap::TITLE)
        .icon_name("avatar-default-symbolic")
        .build();

    // NOTE: identity (actor-id / handle) and the usage & quota group used to live
    // here, but they read cached client state at *build* time — fine in the old
    // modal (rebuilt on each open), stale in the build-once Settings shell. So
    // all live-data display now lives on the Status sub-page (`views::status`,
    // wired to live `StatusHandles` updates in `app.rs`): `account-actor-id`,
    // `quota-section`/`quota-inbox`/`quota-storage`/`quota-devices`,
    // `status-actor-id-copy-btn`. The Account sub-page is pure actions
    // (change-handle, bluesky, export, sign-out, delete) — build-once-safe.
    // See settings.md § Navigation model.

    // error-message — page-level error label (E2E Rule 2), hidden until set.
    // Built here (before every group that can raise one) so the switcher's
    // remove button, the change-handle group below AND the Recovery kit section
    // can all write to the SAME element — ui.yaml lists exactly one
    // `error-message` per page, so a second label with the same id would be the
    // duplicate-id violation convention 1 forbids. It is added into the page
    // below at its original spot, inside change_handle_group, so the visual
    // layout is unchanged.
    let error_label = gtk::Label::builder().visible(false).build();
    error_label.set_halign(gtk::Align::Start);
    error_label.set_wrap(true);
    error_label.add_css_class("error");
    crate::testid::set_test_id(&error_label, ids::ERROR_MESSAGE);
    ACCOUNT_ERROR_LABEL.with(|cell| *cell.borrow_mut() = Some(error_label.downgrade()));

    // --- Accounts switcher group (multi-account) ---
    // Lists the identities held on this client install and lets the user switch
    // between them or add another (long-term-store.md § Multi-account evolution).
    // Reads the shared AccountRegistry directly over the same libsecret/file
    // backend the launch machine uses. Build-once-safe: a switch tears down +
    // rebuilds the whole app (register_switch_account_handler), so this list is
    // rebuilt fresh after every switch — no live update needed for the switch
    // path. A remove WITHOUT a switch does not rebuild the app, so it live-refreshes
    // the group in place (below): the removed row is unparented and its actor is
    // dropped from `switch_targets` in lockstep, keeping the listbox `row-activated`
    // index → account map aligned with the visible rows.
    let switcher_group = adw::PreferencesGroup::builder()
        .title(ap::ACCOUNTS)
        .description(ap::ACCOUNTS_SUBTITLE)
        .build();
    crate::testid::set_test_id(&switcher_group, ids::ACCOUNT_SWITCHER_LIST);

    let registry = crate::account_registry();
    // The account THIS instance serves, not the registry's active pointer: a bound
    // secondary never moves that pointer, and keyed on it the served account read as
    // "not active" and was offered a remove button (account-scoping.md § Concurrent
    // instances; the remove itself is refused in `account_scope::remove_account`).
    let serving = fauna_client_accounts::session_account(&registry);
    let accounts = registry.list();
    // Actor ids in row order — used by the listbox `row-activated` dispatch below
    // (index → account). Shared + mutable so an in-place `remove` can drop the gone
    // account's entry in lockstep with unparenting its row, keeping index → account
    // aligned without a rebuild. The trailing "Add account" row has no entry here.
    let switch_targets = std::rc::Rc::new(std::cell::RefCell::new(
        accounts
            .iter()
            .map(|a| a.actor_id.clone())
            .collect::<Vec<String>>(),
    ));
    // Stage-2 toggles by actor, for the on-visible refresh below. This page is
    // built ONCE (`views::settings_shell`), but the flag can change *after* the
    // build without any app rebuild: the admin auto-default writes it at the
    // am-i-admin observation, seconds after launch. A stale row would then render
    // OFF over a registry that says ON — and the user could never turn the flag off,
    // because their tap on an OFF-looking switch writes ON. Hence the refresh.
    #[allow(clippy::type_complexity)]
    let confirm_toggles: std::rc::Rc<std::cell::RefCell<Vec<(String, gtk::Switch)>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    // Set while the refresh writes a switch's state, so `connect_active_notify` can
    // tell a registry-driven sync from a human tap (the agent flips a `gtk::Switch`
    // programmatically too, so the signal alone cannot distinguish them).
    let confirm_syncing = std::rc::Rc::new(std::cell::Cell::new(false));
    let ctx = SwitcherCtx {
        group: switcher_group.clone(),
        serving: serving.clone(),
        switch_targets: switch_targets.clone(),
        confirm_toggles: confirm_toggles.clone(),
        confirm_syncing: confirm_syncing.clone(),
        error_label: error_label.clone(),
    };
    // The account rows currently in the group, so the on-visible refresh can
    // replace them when the registry's accounts changed under a build-once page.
    let account_rows: std::rc::Rc<std::cell::RefCell<Vec<adw::ActionRow>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    for entry in accounts.into_iter() {
        let row = build_account_row(&ctx, entry);
        switcher_group.add(&row);
        account_rows.borrow_mut().push(row);
    }

    // "Add account" → append-mode onboarding (create-or-import → handle → connect),
    // then switch to the new identity. The whole row activates it.
    let add_row = adw::ActionRow::builder()
        .title(ap::ADD_ACCOUNT)
        .activatable(true)
        .build();
    let add_btn = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    crate::testid::set_test_id(&add_btn, ids::ACCOUNT_ADD_BUTTON);
    add_row.add_suffix(&add_btn);
    // Add via the button (agent-reliable + real users). The row-body click is
    // handled by the listbox `row-activated` dispatch below (trailing index) —
    // NOT a per-row connect_activated, which would double-fire with the listbox
    // path (add has no re-entrancy guard, unlike switch) and pop two wizards.
    add_btn.connect_clicked(|b| launch_add_account_from_widget(b.upcast_ref::<gtk::Widget>()));
    switcher_group.add(&add_row);

    // Wire the group's internal listbox `row-activated` (the rows' parent, valid
    // now that they're added). The e2e agent actuates an AdwActionRow by emitting
    // `row-activated` on this listbox — it does NOT fire AdwActionRow::activated —
    // and real clicks emit it too. Index → account (trailing row = "Add account").
    // The per-row `connect_activated` above additionally covers the agent's
    // `w.activate()` fallback + real users; the switch handler's re-entrancy guard
    // dedupes the redundant fire. (Defensive because the agent's chosen path for an
    // AdwActionRow-in-AdwPreferencesGroup isn't guaranteed — automation/agent.rs.)
    if let Some(listbox) = add_row.parent().and_downcast::<gtk::ListBox>() {
        let targets = switch_targets.clone();
        let serving_id = serving.clone();
        listbox.connect_row_activated(move |_lb, row| {
            let idx = row.index().max(0) as usize;
            // Clone the target out so the RefCell borrow is released before we act (a
            // switch tears down + rebuilds the whole app). `switch_targets` stays
            // aligned with the visible rows across an in-place `remove`, so index →
            // account stays correct.
            let target = targets.borrow().get(idx).cloned();
            match target {
                // Any account but the one this instance serves → switch (Stage-2 gated).
                Some(actor)
                    if !serving_id
                        .as_deref()
                        .is_some_and(|s| s.eq_ignore_ascii_case(&actor)) =>
                {
                    request_switch_account(actor, row.upcast_ref());
                }
                // The served account row — no self-switch.
                Some(_) => {}
                // Past the account rows → the "Add account" row.
                None => launch_add_account_from_widget(row.upcast_ref::<gtk::Widget>()),
            }
        });
    }

    page.add(&switcher_group);

    // --- Identity export group ---
    // The QR a second device scans to import this identity (settings.md § Identity
    // export). Placed before Change handle, mirroring the shipped Apple order.
    page.add(&crate::settings::identity_export::build_identity_export_group());

    // --- Recovery kit group ---
    // The RecoveryKey's Settings home (settings.md § Recovery kit), ratified
    // 2026-08-01 as a section immediately after Identity export — the two are
    // sibling root-secret affordances, not a new rail entry.
    let (recovery_kit_group, recovery_kit_refresh) =
        crate::settings::recovery_kit::build_recovery_kit_group(&error_label);
    page.add(&recovery_kit_group);

    // --- Change handle group ---
    let change_handle_group = adw::PreferencesGroup::builder()
        .title(ap::CHANGE_HANDLE)
        .build();

    let handle_entry = adw::EntryRow::builder().title(ap::NEW_HANDLE).build();
    crate::testid::set_test_id(&handle_entry, ids::NEW_HANDLE);
    change_handle_group.add(&handle_entry);

    let change_btn_row = adw::ActionRow::builder()
        .title(common::APPLY)
        .activatable(true)
        .build();

    let change_btn = gtk::Button::builder()
        .label(common::CHANGE)
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    crate::testid::set_test_id(&change_btn, ids::CHANGE_HANDLE);
    crate::offline_gate::declare_wire_kind(&change_btn, "fauna.profile.handle.change");
    change_btn_row.add_suffix(&change_btn);
    change_handle_group.add(&change_btn_row);
    change_handle_group.add(&error_label);

    let handle_entry_ref = handle_entry.clone();
    let error_label_ref = error_label.clone();
    change_btn.connect_clicked(move |_btn| {
        let new_handle = handle_entry_ref.text();
        // Client-side format validation via the shared canonical validator — the
        // SAME rules the nest enforces (`fauna_protocol::handle::validate_handle`),
        // so feedback is instant and identical across every app. A *taken*
        // handle stays server-authoritative and surfaces from the change RPC reply.
        if let Err(msg) = fauna_protocol::handle::validate_handle(new_handle.as_str()) {
            super::render_account_error_label(&error_label_ref, Some(msg));
            return;
        }
        // `render_account_error_label`, not a bare `set_visible`: a pending
        // stolen-ceremony persist-failure message on this SAME shared label
        // outranks a Change-handle click (`settings.md` § Recovery kit →
        // *The persist-failure message survives the page*; ).
        super::render_account_error_label(&error_label_ref, None);
        if let Some(client) = crate::settings::get_client() {
            client.change_handle(&new_handle);
        } else {
            tracing::error!("[settings/account] change_handle: no client available");
        }
    });

    page.add(&change_handle_group);

    // --- Bridges group (unified) ---
    // Bluesky / ActivityPub / Nostr / Email linking lives on the unified
    // Bridges page (sidebar) over the `fauna.bridges.*` WS-RPC kinds — this
    // page just points there (the legacy account-page Bluesky-OAuth row, which
    // called the now-deleted `/api/v1/bluesky/auth/{start,status}` HTTP twins,
    // was removed). GTK widgets are not Send, so they can't be captured in the
    // background WS-RPC callbacks anyway; live bridge data is fetched on the
    // Bridges view via the dedicated DataMessage::BridgesLoaded path.
    let bridges_group = adw::PreferencesGroup::builder()
        .title(common::BRIDGES)
        .description(ap::BRIDGES_DESCRIPTION)
        .build();

    let bridges_info_row = adw::ActionRow::builder()
        .title(ap::BRIDGE_MANAGEMENT)
        .subtitle(ap::BRIDGE_MANAGEMENT_SUBTITLE)
        .build();
    bridges_group.add(&bridges_info_row);

    page.add(&bridges_group);

    // --- Data group ---
    let data_group = adw::PreferencesGroup::builder().title(ap::DATA).build();

    let export_row = adw::ActionRow::builder()
        .title(ap::EXPORT_MY_DATA)
        .subtitle(ap::EXPORT_SUBTITLE)
        .activatable(true)
        .build();

    let export_btn = gtk::Button::builder()
        .label(common::EXPORT_ACTION)
        .valign(gtk::Align::Center)
        .build();
    crate::testid::set_test_id(&export_btn, ids::SETTINGS_EXPORT_DATA_BUTTON);
    export_row.add_suffix(&export_btn);
    export_btn.connect_clicked(|btn| {
        // Under e2e automation, bypass the native save dialog and write straight
        // to the harness-provided dir — the same `FAUNA_E2E_DOWNLOAD_DIR` seam as
        // backups' file_list.rs, and the same filename tui saves under.
        //
        // Two gates, both required — see file_list.rs's twin of this block for
        // why the compile gate is carried even though the predicate's production
        // twin already folded the branch.
        #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
        if crate::e2e_mode_enabled()
            && let Ok(dir) = std::env::var("FAUNA_E2E_DOWNLOAD_DIR")
        {
            if let Some(client) = crate::settings::get_client() {
                let save_path = std::path::Path::new(&dir).join("fauna-export.zip");
                client.export_account_data(&save_path.display().to_string());
            }
            return;
        }

        let parent = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
        let dialog = gtk::FileDialog::builder()
            .title(ap::EXPORT_DIALOG_TITLE)
            // The nest sends the export as a zip archive
            // (`bins/fauna-nest/src/export_routes.rs` → `application/zip`,
            // `fauna-export-<ts>.zip`), and `export_account_data` writes those
            // bytes verbatim — so suggest a `.zip` name, not `.json`.
            .initial_name("fauna-export.zip")
            .build();
        dialog.save(parent.as_ref(), gio::Cancellable::NONE, move |result| {
            if let Ok(file) = result
                && let Some(path) = file.path()
                && let Some(client) = crate::settings::get_client()
            {
                client.export_account_data(&path.display().to_string());
            }
        });
    });
    data_group.add(&export_row);

    page.add(&data_group);

    // --- Session group ---
    let session_group = adw::PreferencesGroup::builder().title(ap::SESSION).build();

    let sign_out_row = adw::ActionRow::builder()
        .title(crate::i18n::strings::settings::SIGN_OUT)
        .subtitle(ap::SIGN_OUT_SUBTITLE)
        .build();

    let sign_out_btn = gtk::Button::builder()
        .label(crate::i18n::strings::settings::SIGN_OUT)
        .valign(gtk::Align::Center)
        .css_classes(["destructive-action"])
        .build();
    crate::testid::set_test_id(&sign_out_btn, ids::SIGN_OUT_BUTTON);

    let sign_out_error_label = error_label.clone();
    sign_out_btn.connect_clicked(move |btn| {
        let error_label = sign_out_error_label.clone();
        // No wire kind: `trigger_sign_out` is client-local (keyring, window and
        // agent teardown — `ui/settings.md` § Sign out), so there is no write
        // for the offline gate to guard.
        crate::confirm_dialog::present_confirm(
            btn,
            crate::confirm_dialog::ConfirmSpec::new(
                crate::i18n::strings::settings::SIGN_OUT,
                crate::confirm_dialog::ConfirmBody::Text(
                    crate::i18n::strings::settings::SIGN_OUT_CONFIRM,
                ),
                "sign-out",
                crate::i18n::strings::settings::SIGN_OUT,
                common::CANCEL,
            )
            .with_confirm_id(ids::SIGN_OUT_CONFIRM_BUTTON)
            .with_cancel_id(ids::SIGN_OUT_CANCEL_BUTTON),
            move || {
                // The refusal runs on CONFIRM, not on the button: a user who
                // cancels was never signing out, and a line painted before
                // they decided would name a problem they do not have.
                sign_out_confirm_unless(&error_label, crate::account_scope::sign_out_blocked);
            },
        );
    });

    sign_out_row.add_suffix(&sign_out_btn);
    session_group.add(&sign_out_row);
    page.add(&session_group);

    // --- Danger zone ---
    let danger_group = adw::PreferencesGroup::builder()
        .title(common::DANGER_ZONE)
        .build();

    let delete_row = adw::ActionRow::builder()
        .title(ap::DELETE_ACCOUNT)
        .subtitle(ap::DELETE_SUBTITLE)
        .build();
    danger_group.add(&delete_row);

    // Type-to-confirm gate (ui.yaml's `settings-delete-confirm-field`,
    // mirroring web/tui/windows): no separate confirm dialog — the button
    // stays insensitive until the field reads exactly "DELETE", matching
    // every other app's single-click-after-typing shape (priority #1;
    // ui.yaml carves out no dialog variant for this page).
    let delete_confirm_entry = adw::EntryRow::builder()
        .title(crate::i18n::strings::settings::DELETE_CONFIRM_PLACEHOLDER)
        .build();
    crate::testid::set_test_id(&delete_confirm_entry, ids::SETTINGS_DELETE_CONFIRM_FIELD);
    danger_group.add(&delete_confirm_entry);

    let delete_btn = gtk::Button::builder()
        .label(ap::DELETE_ACCOUNT)
        .valign(gtk::Align::Center)
        .css_classes(["destructive-action"])
        .sensitive(false)
        .build();
    crate::testid::set_test_id(&delete_btn, ids::SETTINGS_DELETE_ACCOUNT_BUTTON);
    crate::offline_gate::declare_wire_kind(&delete_btn, "fauna.account.delete");

    let delete_btn_ref = delete_btn.clone();
    delete_confirm_entry.connect_changed(move |entry| {
        delete_btn_ref.set_sensitive(entry.text() == "DELETE");
    });

    delete_btn.connect_clicked(|_btn| {
        if let Some(client) = crate::settings::get_client() {
            client.delete_account();
        } else {
            tracing::error!("[settings/account] delete_account: no client available");
        }
    });

    let delete_btn_row = adw::ActionRow::builder().build();
    delete_btn_row.add_suffix(&delete_btn);
    danger_group.add(&delete_btn_row);
    page.add(&danger_group);

    // --- Pending actions group ---
    // The STANDING cancellation-window section (`settings.md` § Pending
    // actions), sitting below the two delayed verbs this page hosts
    // (change handle, delete account) — mirroring tui's placement. The
    // third delayed verb, snapshot delete, schedules from the Backups page
    // and appears here on the next Account visit's hydrate (this page's own
    // on-visible refresh below).
    let (pending_actions_group, pending_actions_refresh) =
        crate::settings::pending_actions::build_pending_actions_group();
    page.add(&pending_actions_group);

    // Re-sync the Stage-2 toggles from the registry AND re-read the recovery
    // kit status whenever this page becomes visible — see `confirm_toggles`
    // and `recovery_kit_refresh`. Wired to the shell stack's visible-child
    // notify, the same seam the General page uses for its build-time tray cache.
    let refresh: std::rc::Rc<dyn Fn()> = {
        let toggles = confirm_toggles.clone();
        let syncing = confirm_syncing.clone();
        let ctx = ctx.clone();
        let account_rows = account_rows.clone();
        let add_row = add_row.clone();
        std::rc::Rc::new(move || {
            let reg = crate::account_registry();
            // The switcher's rows follow the registry, not the moment the page
            // was built: an account registered after the build (another
            // instance's "Add account", or a sign-in that wrote the registry
            // after the shell came up) must show here the next time the user
            // looks, as tui re-reads its snapshot on every entry. Rebuilt only
            // when the account list actually changed, so an unchanged visit
            // keeps its widgets (and a half-typed toggle) as they are.
            let listed = reg.list();
            let listed_ids: Vec<String> = listed.iter().map(|a| a.actor_id.clone()).collect();
            if listed_ids != *ctx.switch_targets.borrow() {
                for row in account_rows.borrow_mut().drain(..) {
                    if row.parent().is_some() {
                        ctx.group.remove(&row);
                    }
                }
                ctx.group.remove(&add_row);
                ctx.confirm_toggles.borrow_mut().clear();
                *ctx.switch_targets.borrow_mut() = listed_ids;
                for entry in listed {
                    let row = build_account_row(&ctx, entry);
                    ctx.group.add(&row);
                    account_rows.borrow_mut().push(row);
                }
                ctx.group.add(&add_row);
            }
            let flags: std::collections::HashMap<String, bool> = reg
                .list()
                .into_iter()
                .map(|a| (a.actor_id, a.require_confirm_to_activate))
                .collect();
            for (actor, sw) in toggles.borrow().iter() {
                let want = flags.get(actor).copied().unwrap_or(false);
                if sw.is_active() != want {
                    syncing.set(true);
                    sw.set_active(want);
                    syncing.set(false);
                }
                crate::testid::set_test_attr(sw, "state", if want { "on" } else { "off" });
            }
            recovery_kit_refresh();
            pending_actions_refresh();
        })
    };

    (
        crate::testid::wrap_page_with_heading(ap::TITLE, ids::PAGE_HEADING, &page),
        refresh,
    )
}

/// Launch append-mode onboarding ("Add account") from a widget in the running
/// window. Resolves the `adw::Application` from the widget's root window (the
/// account page is built without one, like the export/change-handle buttons) and
/// hands off to `crate::launch_add_account_wizard`.
fn launch_add_account_from_widget(widget: &gtk::Widget) {
    if let Some(app) = widget
        .root()
        .and_then(|r| r.downcast::<gtk::Window>().ok())
        .and_then(|w| w.application())
        .and_then(|a| a.downcast::<adw::Application>().ok())
    {
        crate::launch_add_account_wizard(&app);
    } else {
        tracing::error!("[settings/account] add-account: no application in scope");
    }
}

/// What every switcher row's handlers share with the page — the group they
/// live in, the index → account map the listbox dispatch reads, the Stage-2
/// toggles the on-visible refresh re-syncs, and the page's `error-message`.
#[derive(Clone)]
struct SwitcherCtx {
    group: adw::PreferencesGroup,
    serving: Option<String>,
    switch_targets: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    #[allow(clippy::type_complexity)]
    confirm_toggles: std::rc::Rc<std::cell::RefCell<Vec<(String, gtk::Switch)>>>,
    confirm_syncing: std::rc::Rc<std::cell::Cell<bool>>,
    error_label: gtk::Label,
}

/// One `account-switcher-item` row for `entry` — built at page build and again
/// whenever the on-visible refresh finds the registry's accounts changed.
fn build_account_row(
    ctx: &SwitcherCtx,
    entry: fauna_client_accounts::AccountEntry,
) -> adw::ActionRow {
    let is_active = ctx
        .serving
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case(&entry.actor_id));
    // Title = handle if the server-data cache has it, else a short actor-id
    // prefix (the switcher is usable before the first cache refresh). Shared:
    // `fauna_core::format::account_display_label` (single-sourced so it can't
    // drift — `value-formatting.md` § Account display label).
    let title = fauna_core::format::account_display_label(entry.handle.as_deref(), &entry.actor_id);
    let row = adw::ActionRow::builder()
        .title(&title)
        // The active account is not itself a switch target.
        .activatable(!is_active)
        .build();
    crate::testid::set_test_id(&row, ids::ACCOUNT_SWITCHER_ITEM);

    // `account-item-handle` is the row's own visible title — the handle the
    // user reads. (It used to be a hidden duplicate label, which the automation
    // walk prunes as non-showing, so the id was never found.)
    if !crate::testid::tag_row_title(&row, ids::ACCOUNT_ITEM_HANDLE) {
        tracing::warn!("[settings/account] switcher row has no title label to carry its handle id");
    }

    // Stage-2 "require re-auth to activate" flag — on EVERY row, including the
    // ACTIVE one (unlike `account-remove-button` below, which is non-active-only):
    // the natural target is the user's admin identity, which is usually the
    // account you are already on, and the admin auto-default flags exactly that
    // row. Setting the flag never prompts; only *activating* a flagged account
    // does (long-term-store.md § Multi-account evolution → Per-account re-auth).
    let confirm_toggle = gtk::Switch::builder()
        .active(entry.require_confirm_to_activate)
        .valign(gtk::Align::Center)
        .tooltip_text(ap::REQUIRE_CONFIRM_TOGGLE)
        .build();
    crate::testid::set_test_id(&confirm_toggle, ids::ACCOUNT_REQUIRE_CONFIRM_TOGGLE);
    crate::testid::set_test_attr(
        &confirm_toggle,
        "state",
        if entry.require_confirm_to_activate {
            "on"
        } else {
            "off"
        },
    );
    let actor_for_confirm = entry.actor_id.clone();
    let syncing_for_write = ctx.confirm_syncing.clone();
    confirm_toggle.connect_active_notify(move |sw| {
        let on = sw.is_active();
        crate::testid::set_test_attr(sw, "state", if on { "on" } else { "off" });
        // A programmatic re-sync from the registry is not a human choice — it must
        // not mark `require_confirm_user_set`, or merely *viewing* this page would
        // consume the user's override right and freeze the admin auto-default out.
        if syncing_for_write.get() {
            return;
        }
        let reg = crate::account_registry();
        // The write also marks `require_confirm_user_set`, which is what pins the
        // user's choice against the admin auto-default (an explicit OFF sticks).
        if let Err(e) = reg.set_require_confirm(&actor_for_confirm, on) {
            tracing::error!("[settings/account] set_require_confirm failed: {e:#}");
        }
    });
    row.add_suffix(&confirm_toggle);
    ctx.confirm_toggles
        .borrow_mut()
        .push((entry.actor_id.clone(), confirm_toggle.clone()));

    if is_active {
        let indicator = gtk::Image::from_icon_name("emblem-ok-symbolic");
        indicator.set_tooltip_text(Some(common::ACTIVE));
        crate::testid::set_test_id(&indicator, ids::ACCOUNT_ITEM_ACTIVE_INDICATOR);
        row.add_suffix(&indicator);
    } else {
        // Switch on tap — through the Stage-2 gate, never straight to the seam.
        let actor_for_switch = entry.actor_id.clone();
        row.connect_activated(move |r| {
            request_switch_account(actor_for_switch.clone(), r.upcast_ref());
        });
    }

    // "Open as new instance" — the running instance's concurrent-instances
    // affordance (account-scoping.md § Concurrent instances → the running
    // instance's surface). Offered on EVERY row, active included, since
    // linux's W5.6 (account-data-plane.md § Workstreams) retirement (2026-08-15): the account this instance
    // already serves now accepts a same-account spawn as an ordinary
    // bound launch that COEXISTS (the per-account lock is shared), so the
    // restriction that reserved this button for non-active rows — a spawn
    // onto the served account used to be a guaranteed refusal — has
    // lapsed with the app's retirement.
    //
    // The child owns its whole binding outcome — unknown account,
    // re-auth-flagged account are all decided by `bind_account` over
    // there. Nothing is pre-checked here; that is what keeps one gate
    // instead of two that can drift.
    let spawn_btn = gtk::Button::builder()
        .icon_name("window-new-symbolic")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .tooltip_text(ap::OPEN_NEW_INSTANCE)
        .build();
    crate::testid::set_test_id(&spawn_btn, ids::ACCOUNT_OPEN_NEW_INSTANCE_BUTTON);
    let actor_for_spawn = entry.actor_id.clone();
    spawn_btn.connect_clicked(move |_| {
        crate::instance_remote::spawn_bound_instance(&actor_for_spawn);
    });
    row.add_suffix(&spawn_btn);

    if !is_active {
        // Remove is offered only for non-active accounts (removing the active
        // one would need an immediate re-route — out of Slice 2 scope). This
        // restriction is unrelated to W5.6 and does not lapse with it.
        let remove_btn = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .tooltip_text(common::REMOVE)
            .build();
        crate::testid::set_test_id(&remove_btn, ids::ACCOUNT_REMOVE_BUTTON);
        let actor_for_remove = entry.actor_id.clone();
        // Weak refs: the row owns the button owns this closure, so a strong `row`
        // capture would cycle-leak; the group ref is weak for symmetry (the page
        // may already be gone). An Rc clone of the shared index → account map.
        let group_weak = ctx.group.downgrade();
        let row_weak = row.downgrade();
        let targets_for_remove = ctx.switch_targets.clone();
        let toggles_for_remove = ctx.confirm_toggles.clone();
        let error_label_for_remove = ctx.error_label.clone();
        remove_btn.connect_clicked(move |_| {
            // Refused while another instance serves the account, and a
            // registry failure — both on the page's `error-message`, never
            // only the log (testing.md point 11; the tui twin paints its
            // Settings error line the same way). The order inside is
            // `account_scope::remove_account`'s: ask, then the registry,
            // then the erase.
            remove_account_confirm_unless(
                &actor_for_remove,
                &error_label_for_remove,
                crate::account_scope::remove_account,
                || {
                    // Live-refresh the build-once page: drop this account's
                    // entry from the listbox index → actor map, then unparent
                    // its row — in lockstep, so the trailing "Add account"
                    // dispatch and the remaining switch rows stay correct
                    // without a page rebuild.
                    targets_for_remove
                        .borrow_mut()
                        .retain(|a| a != &actor_for_remove);
                    toggles_for_remove
                        .borrow_mut()
                        .retain(|(a, _)| a != &actor_for_remove);
                    if let (Some(group), Some(row)) = (group_weak.upgrade(), row_weak.upgrade()) {
                        group.remove(&row);
                    }
                },
            );
        });
        row.add_suffix(&remove_btn);
    }
    row
}
