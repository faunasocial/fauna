//! The Settings → **Sessions** sub-page (`docs/goal/ui/sessions.md`; ui.yaml
//! page `sessions`, reached `{"view":"settings","id":"sessions"}`) — where this
//! account is signed in right now, as one escalation ladder: the list (revoke
//! one), sign out everywhere else, the 24-hour lock. The rail slot is directly
//! after Devices (`settings.md` § Navigation model). tui is the lead app
//! (`apps/fauna-tui/src/settings/sessions.rs`); this is the same page, region
//! for region.
//!
//! **A paint shell over shared Rust.** The rows, their order, kinds and marks
//! are `fauna_client_account::sessions_view`'s fold; the act-then-refold pass is
//! `SessionsClient::pass` (a failed re-read keeps the painted rows through
//! `SessionsPass::onto`); the roster input is `sessions_view::roster_from` over
//! the shared Devices machine; the confirm word and its action-arm gate are
//! `LOCKOUT_CONFIRM_WORD` and `sessions_view::lockout_word_refusal`. What stays
//! here is what `ui/sessions.md` § Where logic lives leaves to app glue: the
//! render and the two confirm states. No page machine: the page re-reads on
//! every visit and re-snapshots after each act, and keeps no cache beyond the
//! last painted snapshot.
//!
//! **A landed lock** ends every session this account holds, this app's too. The
//! page stands its acts disabled and the app leaves the shell for its launch
//! flow, whose re-run challenge meets the lock and paints the locked surface
//! (`ui/sessions.md` § User actions; leg 2).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use adw::prelude::*;
use fauna_client_account::sessions_view::{self, SessionsRoster, SessionsSnapshot};
use fauna_client_account::{SessionsAct, SessionsClient, SessionsPass};
use fauna_client_recovery::ceremony::LOCKOUT_CONFIRM_WORD;
use fauna_core::localized::LocalizedText;
use fauna_devices_machine::DevicesMachine;
use fauna_ui_ids as ids;

use crate::i18n::strings::sessions as t;
use crate::testid::set_test_id;

/// The rail label + `add_titled` title (settings_shell.rs).
pub const TITLE: &str = t::TITLE;

/// The token id behind each painted `session-card`, in order — `None` before
/// the first hydrate. Published as the `settings.session_token_ids` state key
/// (tui's twin is its `state_json`), the one thing a test cannot read off the
/// widgets: which nest session each card is.
static PAINTED_TOKEN_IDS: Mutex<Option<Vec<String>>> = Mutex::new(None);

/// `settings.session_token_ids` for the agent's state JSON.
pub fn painted_token_ids_json() -> serde_json::Value {
    match &*PAINTED_TOKEN_IDS.lock().unwrap_or_else(|e| e.into_inner()) {
        Some(ids) => serde_json::json!(ids),
        None => serde_json::Value::Null,
    }
}

struct Widgets {
    error_label: gtk::Label,
    list_box: gtk::Box,
    empty_label: gtk::Label,
    revoke_others_btn: gtk::Button,
    /// `sessions-revoke-others-button`'s reason line, painted by the
    /// possibility seam while the button's answer is blocked.
    revoke_others_reason: gtk::Label,
    revoke_others_confirm_btn: gtk::Button,
    revoke_others_cancel_btn: gtk::Button,
    lockout_entry: adw::EntryRow,
    lockout_btn: gtk::Button,
}

struct Ctx {
    rt: tokio::runtime::Handle,
    nest: Arc<fauna_client::NestClient>,
    devices: Arc<DevicesMachine>,
    snapshot: RefCell<Option<SessionsSnapshot>>,
    /// A refusal the page itself raised (the action-arm word gate) — painted
    /// ahead of the snapshot's error, cleared by the next pass.
    local_error: RefCell<Option<LocalizedText>>,
    revoke_others_armed: RefCell<bool>,
    busy: RefCell<bool>,
    /// The lock landed: every session is gone, this app's too.
    locked: RefCell<bool>,
    w: Widgets,
}

/// Build the Sessions preferences page. Every static ui.yaml ID is constructed
/// even when no client is registered (the clientless unit test); only the
/// async wiring is gated on a live client.
pub fn build_sessions_page(devices: Arc<DevicesMachine>) -> (adw::PreferencesPage, impl Fn()) {
    let page = adw::PreferencesPage::builder()
        .title(t::TITLE)
        .icon_name("system-users-symbolic")
        .build();

    // --- Heading + page-level error ---
    let top_group = adw::PreferencesGroup::builder().title(t::TITLE).build();
    top_group.set_header_suffix(Some(&super::marker(ids::PAGE_HEADING)));
    let error_label = gtk::Label::builder().visible(false).build();
    error_label.add_css_class("error");
    error_label.set_halign(gtk::Align::Start);
    error_label.set_wrap(true);
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    top_group.add(&error_label);
    page.add(&top_group);

    // --- 1. The list ---
    let list_group = adw::PreferencesGroup::new();
    let list_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    let empty_label = text_label(t::EMPTY);
    empty_label.set_visible(false);
    set_test_id(&empty_label, ids::SESSIONS_EMPTY);
    list_group.add(&empty_label);
    list_group.add(&list_box);
    page.add(&list_group);

    // --- 2. Sign out everywhere else — the two-press inline confirm ---
    let others_group = adw::PreferencesGroup::new();
    let others_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let revoke_others_btn = gtk::Button::with_label(t::REVOKE_OTHERS);
    set_test_id(&revoke_others_btn, ids::SESSIONS_REVOKE_OTHERS_BUTTON);
    let revoke_others_confirm_btn = gtk::Button::builder()
        .label(t::REVOKE_OTHERS_CONFIRM)
        .css_classes(["destructive-action"])
        .visible(false)
        .build();
    set_test_id(
        &revoke_others_confirm_btn,
        ids::SESSIONS_REVOKE_OTHERS_CONFIRM_BUTTON,
    );
    let revoke_others_cancel_btn = gtk::Button::builder()
        .label(t::REVOKE_OTHERS_CANCEL)
        .visible(false)
        .build();
    set_test_id(
        &revoke_others_cancel_btn,
        ids::SESSIONS_REVOKE_OTHERS_CANCEL_BUTTON,
    );
    others_row.append(&revoke_others_btn);
    others_row.append(&revoke_others_confirm_btn);
    others_row.append(&revoke_others_cancel_btn);
    others_group.add(&others_row);
    // Not live before the roster folds, after a failed first read, with no
    // other sign-in to end, nor while an act is in flight
    // (`sessions_view::revoke_others_possibility`; `ui/sessions.md` § Errors &
    // edge cases). The reason sits right under the button.
    let revoke_others_reason = text_label("");
    revoke_others_reason.set_visible(false);
    revoke_others_reason.add_css_class("dim-label");
    revoke_others_reason.add_css_class("caption");
    crate::possible::paint(
        &revoke_others_btn,
        Some(&revoke_others_reason),
        sessions_view::revoke_others_possibility(None, false),
    );
    others_group.add(&revoke_others_reason);
    let note = text_label(t::REVOKE_NOTE);
    note.add_css_class("caption");
    set_test_id(&note, ids::SESSIONS_REVOKE_NOTE);
    others_group.add(&note);
    page.add(&others_group);

    // --- 3. The lock — the warning always visible, never behind the confirm ---
    let lock_group = adw::PreferencesGroup::new();
    let warning = text_label(t::LOCKOUT_WARNING);
    set_test_id(&warning, ids::SESSIONS_LOCKOUT_WARNING);
    lock_group.add(&warning);
    let lockout_entry = adw::EntryRow::builder()
        .title(t::LOCKOUT_CONFIRM_PLACEHOLDER)
        .build();
    set_test_id(&lockout_entry, ids::SESSIONS_LOCKOUT_CONFIRM_FIELD);
    lock_group.add(&lockout_entry);
    let lockout_btn = gtk::Button::builder()
        .label(t::LOCKOUT_BUTTON)
        .css_classes(["destructive-action"])
        .halign(gtk::Align::Start)
        .sensitive(false)
        .build();
    set_test_id(&lockout_btn, ids::SESSIONS_LOCKOUT_BUTTON);
    lock_group.add(&lockout_btn);
    page.add(&lock_group);

    let widgets = Widgets {
        error_label,
        list_box,
        empty_label,
        revoke_others_btn,
        revoke_others_reason,
        revoke_others_confirm_btn,
        revoke_others_cancel_btn,
        lockout_entry,
        lockout_btn,
    };
    let visit = wire(widgets, devices);
    let refresh = move || {
        if let Some(visit) = &visit {
            visit();
        }
    };
    (page, refresh)
}

/// Wire the gestures and return the closure each visit runs. `None` (the page
/// stays static) when no client is registered.
fn wire(widgets: Widgets, devices: Arc<DevicesMachine>) -> Option<impl Fn()> {
    let client = super::get_client()?;
    let ctx = Rc::new(Ctx {
        rt: client.runtime_handle(),
        nest: Arc::clone(client.nest_rpc()),
        devices,
        snapshot: RefCell::new(None),
        local_error: RefCell::new(None),
        revoke_others_armed: RefCell::new(false),
        busy: RefCell::new(false),
        locked: RefCell::new(false),
        w: widgets,
    });

    {
        let ctx_ = Rc::clone(&ctx);
        ctx.w
            .lockout_entry
            .connect_changed(move |_| paint_controls(&ctx_));
    }
    {
        let ctx_ = Rc::clone(&ctx);
        ctx.w.revoke_others_btn.connect_clicked(move |_| {
            // Arming asks the answer the button painted from: before the
            // roster, or with nothing to end, there is nothing to confirm.
            if refuse_revoke_others(&ctx_) {
                return;
            }
            *ctx_.revoke_others_armed.borrow_mut() = true;
            paint_controls(&ctx_);
        });
    }
    {
        let ctx_ = Rc::clone(&ctx);
        ctx.w.revoke_others_cancel_btn.connect_clicked(move |_| {
            *ctx_.revoke_others_armed.borrow_mut() = false;
            paint_controls(&ctx_);
        });
    }
    {
        let ctx_ = Rc::clone(&ctx);
        ctx.w.revoke_others_confirm_btn.connect_clicked(move |_| {
            *ctx_.revoke_others_armed.borrow_mut() = false;
            // The confirm asks the same answer again (the roster may have
            // changed, or an act gone out, since the arm).
            if refuse_revoke_others(&ctx_) {
                return;
            }
            run(&ctx_, Some(SessionsAct::RevokeOthers));
        });
    }
    {
        let ctx_ = Rc::clone(&ctx);
        ctx.w.lockout_btn.connect_clicked(move |_| {
            // Re-checked here and not only in the render: an agent driving the
            // id directly can reach the arm (`ui/sessions.md` § The ruling, 3).
            let typed = ctx_.w.lockout_entry.text();
            if let Some(refusal) = sessions_view::lockout_word_refusal(&typed, LOCKOUT_CONFIRM_WORD)
            {
                *ctx_.local_error.borrow_mut() = Some(refusal);
                paint_error(&ctx_);
                return;
            }
            run(&ctx_, Some(SessionsAct::Lockout));
        });
    }

    Some(move || {
        // A fresh visit: nothing armed, no typed word kept across a navigation.
        *ctx.revoke_others_armed.borrow_mut() = false;
        ctx.w.lockout_entry.set_text("");
        run(&ctx, None);
    })
}

/// One pass — act (if asked), re-read, fold — off the GTK thread, then paint.
fn run(ctx: &Rc<Ctx>, act: Option<SessionsAct>) {
    if *ctx.busy.borrow() || *ctx.locked.borrow() {
        return;
    }
    *ctx.busy.borrow_mut() = true;
    paint_controls(ctx);
    let nest = Arc::clone(&ctx.nest);
    let devices = Arc::clone(&ctx.devices);
    let ctx = Rc::clone(ctx);
    crate::async_helper::spawn_with_snapshot(
        &ctx.rt.clone(),
        move || async move {
            let client = SessionsClient::new(Arc::clone(&nest), nest);
            client
                .pass(act, roster(devices), || {
                    fauna_launch_machine::launch_clock::now_secs_or_zero().max(0) as u64
                })
                .await
        },
        move |pass: SessionsPass| {
            *ctx.busy.borrow_mut() = false;
            *ctx.local_error.borrow_mut() = None;
            let locked = pass.locked;
            if locked {
                *ctx.locked.borrow_mut() = true;
            }
            let snapshot = pass.onto(ctx.snapshot.borrow_mut().take());
            *ctx.snapshot.borrow_mut() = Some(snapshot);
            paint(&ctx);
            if locked {
                // Every token — this app's too — is gone: leave the shell for
                // the launch flow, which lands the locked surface.
                crate::settings::escalate_to_launch("account-locked");
            }
        },
    );
}

/// The roster names device-minted sessions — rendered by the shared Devices
/// machine, which owns the sealed-label render; this page never touches keys.
/// This machine's row is the same read door as the Devices page's This-device
/// marker (`devices.md` § This-device marker).
async fn roster(devices: Arc<DevicesMachine>) -> SessionsRoster {
    devices.refresh().await;
    let rows = devices.snapshot().devices;
    let enrolled = match crate::account_runtime::handle() {
        Some(store) => store.enrolled_device_row().await.ok().flatten(),
        None => None,
    };
    let local = crate::sync::device_id()
        .ok()
        .map(|d| fauna_core::hex32::encode(&d));
    let this_row = fauna_devices_machine::this_device_row(enrolled.as_deref(), local.as_deref());
    sessions_view::roster_from(
        rows.into_iter()
            .map(|d| (d.device_id, d.principal, d.label)),
        this_row.as_deref(),
    )
}

fn paint(ctx: &Rc<Ctx>) {
    paint_rows(ctx);
    paint_error(ctx);
    paint_controls(ctx);
}

fn paint_rows(ctx: &Rc<Ctx>) {
    let list = &ctx.w.list_box;
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let snapshot = ctx.snapshot.borrow();
    let Some(snapshot) = snapshot.as_ref() else {
        // Un-hydrated paints nothing rather than a claim.
        ctx.w.empty_label.set_visible(false);
        *PAINTED_TOKEN_IDS.lock().unwrap_or_else(|e| e.into_inner()) = None;
        return;
    };
    ctx.w.empty_label.set_visible(snapshot.rows.is_empty());
    for row in &snapshot.rows {
        let card = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        card.add_css_class("card");
        set_test_id(&card, ids::SESSION_CARD);

        let head = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        let kind = text_label(&resolve(&row.kind));
        kind.add_css_class("heading");
        set_test_id(&kind, ids::SESSION_KIND);
        head.append(&kind);
        if let Some(mark) = &row.mark {
            let badge = text_label(&resolve(mark));
            badge.add_css_class("accent");
            set_test_id(&badge, ids::SESSION_THIS_MARK_BADGE);
            head.append(&badge);
        }
        card.append(&head);

        let detail = text_label(&resolve(&sessions_view::session_detail(
            row,
            format_time,
            crate::i18n::strings::lookup,
        )));
        detail.add_css_class("caption");
        set_test_id(&detail, ids::SESSION_DETAIL);
        card.append(&detail);

        // Absent on this app's own row: leaving this app is Sign out.
        if row.can_revoke {
            let revoke = gtk::Button::builder()
                .label(t::REVOKE)
                .halign(gtk::Align::Start)
                .sensitive(!*ctx.busy.borrow() && !*ctx.locked.borrow())
                .build();
            set_test_id(&revoke, ids::SESSION_REVOKE_BUTTON);
            let ctx_ = Rc::clone(ctx);
            let token_id = row.token_id.clone();
            revoke.connect_clicked(move |_| {
                run(&ctx_, Some(SessionsAct::Revoke(token_id.clone())));
            });
            card.append(&revoke);
        }
        list.append(&card);
    }
    *PAINTED_TOKEN_IDS.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(snapshot.rows.iter().map(|r| r.token_id.clone()).collect());
}

fn paint_error(ctx: &Rc<Ctx>) {
    let text = ctx
        .local_error
        .borrow()
        .clone()
        .or_else(|| ctx.snapshot.borrow().as_ref().and_then(|s| s.error.clone()))
        .map(|e| resolve(&e))
        .filter(|e| !e.is_empty());
    super::render_error_label(&ctx.w.error_label, text.as_deref());
}

/// `sessions-revoke-others-button`'s answer over this page's fold and its
/// in-flight act (a landed lock counts as one: nothing is left to end).
fn revoke_others_answer(ctx: &Ctx) -> fauna_core::possibility::Possibility {
    let busy = *ctx.busy.borrow() || *ctx.locked.borrow();
    sessions_view::revoke_others_possibility(ctx.snapshot.borrow().as_ref(), busy)
}

/// The refusal a revoke-others press (the arm or the confirm) meets, if any:
/// the blocked answer's reason, `None` while the press may go ahead.
fn revoke_others_refusal(snapshot: Option<&SessionsSnapshot>, busy: bool) -> Option<LocalizedText> {
    sessions_view::revoke_others_possibility(snapshot, busy)
        .reason()
        .cloned()
}

/// Ask the answer at a press: refused → the reason on `error-message`, the
/// arm dropped, and `true`.
fn refuse_revoke_others(ctx: &Rc<Ctx>) -> bool {
    let busy = *ctx.busy.borrow() || *ctx.locked.borrow();
    let refusal = revoke_others_refusal(ctx.snapshot.borrow().as_ref(), busy);
    let Some(reason) = refusal else {
        return false;
    };
    *ctx.revoke_others_armed.borrow_mut() = false;
    *ctx.local_error.borrow_mut() = Some(reason);
    paint_error(ctx);
    paint_controls(ctx);
    true
}

/// The two confirm states and the busy/locked gate.
fn paint_controls(ctx: &Rc<Ctx>) {
    let idle = !*ctx.busy.borrow() && !*ctx.locked.borrow();
    let armed = *ctx.revoke_others_armed.borrow();
    // Painted from its answer; armed, the confirm pair takes over, so the
    // button is narrowed further (narrowing never contradicts the answer).
    crate::possible::paint(
        &ctx.w.revoke_others_btn,
        Some(&ctx.w.revoke_others_reason),
        revoke_others_answer(ctx),
    );
    if armed {
        ctx.w.revoke_others_btn.set_sensitive(false);
    }
    ctx.w.revoke_others_confirm_btn.set_visible(armed);
    ctx.w.revoke_others_confirm_btn.set_sensitive(idle);
    ctx.w.revoke_others_cancel_btn.set_visible(armed);
    ctx.w
        .lockout_btn
        .set_sensitive(idle && ctx.w.lockout_entry.text() == LOCKOUT_CONFIRM_WORD);
    let mut child = ctx.w.list_box.first_child();
    while let Some(card) = child {
        let mut inner = card.first_child();
        while let Some(w) = inner {
            if let Ok(button) = w.clone().downcast::<gtk::Button>() {
                button.set_sensitive(idle);
            }
            inner = w.next_sibling();
        }
        child = card.next_sibling();
    }
}

fn text_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_halign(gtk::Align::Start);
    label.set_xalign(0.0);
    label.set_wrap(true);
    label
}

fn resolve(text: &LocalizedText) -> String {
    text.clone().resolve(crate::i18n::strings::lookup)
}

fn format_time(secs: u64) -> String {
    fauna_core::format::format_unix_local(i64::try_from(secs).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rules 2, 5 and 7 on sign out everywhere else (`ui/sessions.md`
    /// § Errors & edge cases): built before the roster folds it is disabled
    /// with its reason beside it, and both presses (the arm and the confirm)
    /// meet a refusal until there is another sign-in to end.
    #[test]
    fn revoke_others_waits_for_the_roster() {
        assert!(revoke_others_refusal(None, false).is_some());
        let only_mine = SessionsSnapshot::default();
        assert!(revoke_others_refusal(Some(&only_mine), false).is_some());
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let devices = DevicesMachine::new(
                Arc::new(fauna_devices_machine::observer::NullObserver)
                    as Arc<dyn fauna_devices_machine::DevicesObserver>,
                Arc::new(fauna_devices_machine::FakeDevicesNestApi::new()),
                Arc::new(fauna_devices_machine::FakeWizardFactory::new()),
            );
            let (page, _refresh) = build_sessions_page(devices);
            let button = crate::testid::find_by_test_id(&page, ids::SESSIONS_REVOKE_OTHERS_BUTTON)
                .expect("the button is built");
            assert!(button.is_visible() && !button.is_sensitive());
            let answer = crate::possible::painted_answer(&button).expect("painted");
            assert_eq!(
                answer.reason().map(|r| r.key.as_str()),
                Some("common.still_loading")
            );
        });
    }
}
