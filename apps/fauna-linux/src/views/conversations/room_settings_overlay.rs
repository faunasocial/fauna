//! The room policy editor — ui.yaml `conversations.sub_pages.room_settings`.
//!
//! Opened by `thread-room-settings-button`; presented as a modal
//! `gtk::Window` over the detail pane. **Not** the `adw::MessageDialog`
//! `rename_overlay` and `add_participant_overlay` use: a MessageDialog closes
//! itself the moment a response fires, and this editor must outlive its own
//! Save — it "closes only when all landed" (`ui/conversations.md` § Element
//! IDs), staying open with the refused value still staged otherwise. Esc
//! cancels, like rename, so there is no cancel id.
//!
//! **Nothing here decides anything.** The staged state, which rows either
//! control may act on, the at-most-one-staged hand-over rule and the diff
//! back to commits are all `fauna_conversations::RoomSettingsDraft`'s, and
//! Save is one `ConversationsManager::apply_room_settings` call — so this
//! file is a painter, and the seven apps cannot drift on the policy rules
//! (priority #2; `conversation-rooms.md` § Roles and authorization).

use adw::prelude::*;
use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;

/// The member-row repainter, held in a slot so it can hand ITSELF to each
/// row's gesture: a staging gesture mutates the shared draft and then
/// rebuilds every row off it, which is what keeps the `checked` attributes
/// and the hand-over's at-most-one rule true without per-widget tracking.
type Repaint = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

use fauna_conversations::{
    HistoryPolicy, JoinRule, RoomSettingsDraft, TypedAddress, history_policy_label,
    join_rule_label, snapshot::ThreadDetail,
};

/// The two token pickers, built the `event-detail-reminder-select` way: the
/// model holds the driver-facing TOKENS (so `select`/`get_text` round-trip
/// them, the cross-app contract) and a label expression paints the localized
/// sentence over them.
fn token_dropdown(tokens: &[&str], labels: fn(&str) -> &'static str, id: &str) -> gtk::DropDown {
    let model = gtk::StringList::new(tokens);
    let dropdown = gtk::DropDown::new(Some(model), gtk::Expression::NONE);
    let label_expr = gtk::ClosureExpression::new::<String>(
        &[] as &[gtk::Expression],
        gtk::glib::closure!(move |item: gtk::StringObject| {
            labels(item.string().as_str()).to_string()
        }),
    );
    dropdown.set_expression(Some(&label_expr));
    crate::testid::set_test_id(&dropdown, id);
    dropdown
}

fn join_rule_label_for_token(token: &str) -> &'static str {
    JoinRule::from_token(token)
        .map(join_rule_label)
        .unwrap_or("")
}

fn history_policy_label_for_token(token: &str) -> &'static str {
    HistoryPolicy::from_token(token)
        .map(history_policy_label)
        .unwrap_or("")
}

fn select_token(dropdown: &gtk::DropDown, token: &str) {
    let model = dropdown
        .model()
        .and_then(|m| m.downcast::<gtk::StringList>().ok());
    let Some(model) = model else { return };
    for i in 0..model.n_items() {
        if model.string(i).map(|s| s == token).unwrap_or(false) {
            dropdown.set_selected(i);
            return;
        }
    }
}

fn selected_token(dropdown: &gtk::DropDown) -> String {
    dropdown
        .selected_item()
        .and_then(|o| o.downcast::<gtk::StringObject>().ok())
        .map(|s| s.string().to_string())
        .unwrap_or_default()
}

/// One member's row: the admin switch and the hand-over control, indexed like
/// the chips. Both are painted for every member and merely GREYED where the
/// viewer's capability or the row's own eligibility says no — never hidden,
/// and never live on the owner's own row (`ui/conversations.md`
/// § Architectural rules 5; the eligibility itself is the shared draft's).
fn member_row(
    index: usize,
    display: &str,
    draft: &Rc<RefCell<RoomSettingsDraft>>,
    participants: &Rc<Vec<TypedAddress>>,
    can_appoint_admins: bool,
    can_transfer_ownership: bool,
    repaint: &Rc<dyn Fn()>,
) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    let name = gtk::Label::new(Some(display));
    name.set_halign(gtk::Align::Start);
    name.set_hexpand(true);
    row.append(&name);

    // Resolved through the draft's identity column rather than indexed into
    // its vectors — `RoomSettingsDraft`'s own doc carries the argument, and it
    // is the same call the tui painter makes.
    let d = draft.borrow();
    let eligible = d.is_eligible(index, participants);
    let staged_admin = d.admin_at(index, participants);
    let staged_owner = d.transfer_staged_at(index, participants);
    drop(d);

    let admin_btn = gtk::ToggleButton::with_label(if staged_admin {
        crate::i18n::strings::conversations::unified::ROOM_ADMIN_YES
    } else {
        crate::i18n::strings::conversations::unified::ROOM_ADMIN_NO
    });
    admin_btn.set_active(staged_admin);
    admin_btn.set_sensitive(can_appoint_admins && eligible);
    crate::testid::set_test_id(&admin_btn, ids::ROOM_ADMIN_TOGGLE);
    crate::testid::set_test_attr(
        &admin_btn,
        "checked",
        if staged_admin { "true" } else { "false" },
    );
    {
        let draft = Rc::clone(draft);
        let participants = Rc::clone(participants);
        let repaint = Rc::clone(repaint);
        admin_btn.connect_clicked(move |_| {
            draft.borrow_mut().toggle_admin(index, &participants);
            repaint();
        });
    }
    row.append(&admin_btn);

    let transfer_btn = gtk::ToggleButton::with_label(if staged_owner {
        crate::i18n::strings::conversations::unified::ROOM_TRANSFER_STAGED
    } else {
        crate::i18n::strings::conversations::unified::ROOM_TRANSFER_MARK
    });
    transfer_btn.set_active(staged_owner);
    transfer_btn.set_sensitive(can_transfer_ownership && eligible);
    crate::testid::set_test_id(&transfer_btn, ids::ROOM_OWNER_TRANSFER_BUTTON);
    crate::testid::set_test_attr(
        &transfer_btn,
        "checked",
        if staged_owner { "true" } else { "false" },
    );
    {
        let draft = Rc::clone(draft);
        let participants = Rc::clone(participants);
        let repaint = Rc::clone(repaint);
        transfer_btn.connect_clicked(move |_| {
            draft.borrow_mut().toggle_transfer(index, &participants);
            repaint();
        });
    }
    row.append(&transfer_btn);

    row
}

/// A live editor: the caller keeps it to report a refusal (which leaves the
/// editor open, its refused value still staged) or the all-landed close.
pub struct RoomSettingsEditor {
    window: gtk::Window,
    error_label: gtk::Label,
}

impl RoomSettingsEditor {
    /// Every staged commit landed — close (`ui/conversations.md` § Element
    /// IDs: "closes only when all landed").
    pub fn close(&self) {
        self.window.close();
    }

    /// A commit was refused. The editor STAYS OPEN with the refused value
    /// still staged, and the page's own `error-message` says which.
    pub fn show_error(&self, message: &str) {
        self.error_label.set_text(message);
        self.error_label.set_visible(true);
    }
}

/// Show the room settings editor over the toplevel window of `parent`.
///
/// `on_save` receives the staged draft; the caller turns it into edits
/// (`RoomSettingsDraft::edits`) and hands them to
/// `ConversationsManager::apply_room_settings`, then calls `close()` or
/// `show_error()` on the returned editor — which is why this is a plain modal
/// `gtk::Window` and not the `adw::MessageDialog` rename uses: a
/// MessageDialog closes itself the moment a response fires, and this editor
/// must outlive its own Save until every commit has landed.
///
/// `None` on a policy-less room or a non-room (no policy to edit) — the button is
/// greyed there, so this is the belt behind that gate.
pub fn show<F: Fn(RoomSettingsDraft) + 'static>(
    parent: &impl IsA<gtk::Widget>,
    detail: &ThreadDetail,
    on_save: F,
) -> Option<Rc<RoomSettingsEditor>> {
    let seed = RoomSettingsDraft::seed(detail)?;
    let toplevel = parent.root().and_then(|r| r.downcast::<gtk::Window>().ok());
    let window = gtk::Window::builder()
        .modal(true)
        .title(crate::i18n::strings::conversations::unified::THREAD_ROOM_SETTINGS)
        .build();
    window.set_transient_for(toplevel.as_ref());
    // Esc cancels, like the rename overlay (no cancel id —
    // `ui/conversations.md` § Element IDs).
    let esc = gtk::EventControllerKey::new();
    {
        let window = window.clone();
        esc.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                window.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    window.add_controller(esc);

    let draft = Rc::new(RefCell::new(seed));
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);

    let join_rule_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let join_rule_caption = gtk::Label::new(Some(
        crate::i18n::strings::conversations::unified::ROOM_JOIN_RULE_LABEL,
    ));
    join_rule_caption.set_halign(gtk::Align::Start);
    join_rule_caption.set_hexpand(true);
    join_rule_row.append(&join_rule_caption);
    let join_rule_tokens: Vec<&str> = JoinRule::EDITOR_CHOICES.iter().map(|r| r.token()).collect();
    let join_rule_dd = token_dropdown(
        &join_rule_tokens,
        join_rule_label_for_token,
        ids::ROOM_JOIN_RULE_SELECT,
    );
    select_token(&join_rule_dd, draft.borrow().join_rule.token());
    join_rule_row.append(&join_rule_dd);
    content.append(&join_rule_row);

    let history_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let history_caption = gtk::Label::new(Some(
        crate::i18n::strings::conversations::unified::ROOM_HISTORY_POLICY_LABEL,
    ));
    history_caption.set_halign(gtk::Align::Start);
    history_caption.set_hexpand(true);
    history_row.append(&history_caption);
    let history_tokens: Vec<&str> = HistoryPolicy::EDITOR_CHOICES
        .iter()
        .map(|p| p.token())
        .collect();
    let history_dd = token_dropdown(
        &history_tokens,
        history_policy_label_for_token,
        ids::ROOM_HISTORY_POLICY_SELECT,
    );
    select_token(&history_dd, draft.borrow().history_policy.token());
    history_row.append(&history_dd);
    content.append(&history_row);

    // The per-member rows are torn down and rebuilt on every staging gesture,
    // so the `checked` attributes and the hand-over's at-most-one rule are
    // always read straight off the draft rather than tracked per widget.
    let members_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    content.append(&members_box);

    let error_label = gtk::Label::new(None);
    error_label.set_halign(gtk::Align::Start);
    error_label.add_css_class("error");
    crate::testid::set_test_id(&error_label, ids::ERROR_MESSAGE);
    error_label.set_visible(false);
    content.append(&error_label);

    let displays: Vec<String> = detail.participant_displays.clone();
    // The list the painted rows are indexed against — handed to the draft so
    // every gesture and every read resolves by identity rather than position
    // (`RoomSettingsDraft::slot_of`).
    let participants: Rc<Vec<TypedAddress>> = Rc::new(detail.participants.clone());
    let can_appoint_admins = detail.capabilities.can_appoint_admins;
    let can_transfer_ownership = detail.capabilities.can_transfer_ownership;

    let repaint: Repaint = Rc::new(RefCell::new(None));
    {
        let members_box = members_box.clone();
        let draft = Rc::clone(&draft);
        let participants = Rc::clone(&participants);
        let repaint_slot = Rc::clone(&repaint);
        let f: Rc<dyn Fn()> = Rc::new(move || {
            while let Some(child) = members_box.first_child() {
                members_box.remove(&child);
            }
            let inner = repaint_slot
                .borrow()
                .clone()
                .expect("the repaint closure installs itself before any gesture can fire");
            for (i, display) in displays.iter().enumerate() {
                members_box.append(&member_row(
                    i,
                    display,
                    &draft,
                    &participants,
                    can_appoint_admins,
                    can_transfer_ownership,
                    &inner,
                ));
            }
        });
        *repaint.borrow_mut() = Some(Rc::clone(&f));
        f();
    }

    let save_btn = gtk::Button::with_label(crate::i18n::strings::common::SAVE);
    save_btn.add_css_class("suggested-action");
    save_btn.set_halign(gtk::Align::End);
    crate::testid::set_test_id(&save_btn, ids::ROOM_SETTINGS_SAVE_BUTTON);
    // Every edit Save issues is a policy commit on the channel — one constant
    // kind for all five, so the button itself declares it.
    crate::offline_gate::declare_wire_kind(&save_btn, "fauna.conversations.channel.send");
    content.append(&save_btn);

    {
        let draft = Rc::clone(&draft);
        save_btn.connect_clicked(move |_| {
            // The two pickers are read at Save (the dropdowns hold the staged
            // tokens; the toggles staged straight into the draft as they were
            // pressed), so the whole staged state reaches the caller as one
            // draft. The window deliberately stays open: the caller closes it
            // once every commit has landed.
            let mut staged = draft.borrow().clone();
            staged.set_join_rule_token(&selected_token(&join_rule_dd));
            staged.set_history_policy_token(&selected_token(&history_dd));
            on_save(staged);
        });
    }

    content.set_margin_start(12);
    content.set_margin_end(12);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    window.set_child(Some(&content));
    window.present();

    Some(Rc::new(RoomSettingsEditor {
        window,
        error_label,
    }))
}
