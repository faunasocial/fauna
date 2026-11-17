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

use super::room_labelers::{LabelerSeams, LabelerSection};
use adw::prelude::*;
use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The member-row repainter, held in a slot so it can hand ITSELF to each
/// row's gesture: a staging gesture mutates the shared draft and then
/// rebuilds every row off it, which is what keeps the `checked` attributes
/// and the hand-over's at-most-one rule true without per-widget tracking.
type Repaint = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

use fauna_conversations::{
    HistoryPolicy, JoinRule, RoomSettingsDraft, ThreadControl, TypedAddress, history_policy_label,
    join_rule_label, snapshot::ThreadDetail,
};
use fauna_core::possibility::Possibility;

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
/// the chips. Each is painted from its shared answer (`AppointAdmins`,
/// `TransferOwnership`): absent for a viewer whose role does not hold it, and
/// where shown, on every member's row — the owner's own row and any other
/// ineligible row dead — so `[i]` stays the chip's index (`ui/conversations.md`
/// § Errors & edge cases → *Controls this person can never use*; the
/// eligibility itself is the shared draft's).
fn member_row(
    index: usize,
    display: &str,
    draft: &Rc<RefCell<RoomSettingsDraft>>,
    participants: &Rc<Vec<TypedAddress>>,
    appoint: &Possibility,
    transfer: &Possibility,
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
    crate::possible::paint(&admin_btn, None, appoint.clone());
    if !eligible {
        admin_btn.set_sensitive(false);
    }
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
        let may = appoint.is_possible();
        admin_btn.connect_clicked(move |_| {
            if !may {
                return;
            }
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
    crate::possible::paint(&transfer_btn, None, transfer.clone());
    if !eligible {
        transfer_btn.set_sensitive(false);
    }
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
        let may = transfer.is_possible();
        transfer_btn.connect_clicked(move |_| {
            if !may {
                return;
            }
            draft.borrow_mut().toggle_transfer(index, &participants);
            repaint();
        });
    }
    row.append(&transfer_btn);

    row
}

/// `room-pending-invite[i]` + `room-pending-invite-withdraw-button[i]`, headed
/// `conversations.unified.room_pending_invites_label`. Every row is one the
/// nest served *because* this viewer may withdraw it, so the button is live on
/// every row and no capability is consulted; the sentence is shared
/// (`RoomPendingInviteSnapshot::text`) and `lapsed` is the driver's attribute.
/// Withdrawing acts at once, outside Save's staged set, with no confirm
/// (inviting again undoes it).
fn paint_pending(pending_box: &gtk::Box, detail: &ThreadDetail, on_withdraw: &Rc<dyn Fn(String)>) {
    while let Some(child) = pending_box.first_child() {
        pending_box.remove(&child);
    }
    let pending = detail
        .room
        .as_ref()
        .and_then(|room| room.pending_invites.as_deref())
        .filter(|pending| !pending.is_empty());
    let Some(pending) = pending else {
        pending_box.set_visible(false);
        return;
    };
    pending_box.set_visible(true);
    let heading = gtk::Label::new(Some(
        crate::i18n::strings::conversations::unified::ROOM_PENDING_INVITES_LABEL,
    ));
    heading.set_halign(gtk::Align::Start);
    heading.add_css_class("heading");
    pending_box.append(&heading);
    for invite in pending {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let text = gtk::Label::new(Some(&invite.text()));
        text.set_halign(gtk::Align::Start);
        text.set_hexpand(true);
        text.set_wrap(true);
        text.set_xalign(0.0);
        crate::testid::set_test_id(&text, ids::ROOM_PENDING_INVITE);
        crate::testid::set_test_attr(
            &text,
            "lapsed",
            if invite.lapsed { "true" } else { "false" },
        );
        row.append(&text);

        let withdraw = gtk::Button::with_label(
            crate::i18n::strings::conversations::unified::ROOM_PENDING_INVITE_WITHDRAW,
        );
        crate::testid::set_test_id(&withdraw, ids::ROOM_PENDING_INVITE_WITHDRAW_BUTTON);
        crate::offline_gate::declare_wire_kind(&withdraw, "fauna.conversations.room.revoke_invite");
        {
            let on_withdraw = Rc::clone(on_withdraw);
            let invitee = invite.invitee_actor_hex.clone();
            withdraw.connect_clicked(move |_| on_withdraw(invitee.clone()));
        }
        row.append(&withdraw);
        pending_box.append(&row);
    }
}

/// The editor's two immediate acts, painted from their shared answers and
/// re-painted off every live tick: the walk-out (`LeaveRoom`, its blocked
/// reason on `leave_reason`) and the local remove (`LocalRemove`). The cells
/// hold what each click re-asks.
fn paint_departures(
    detail: &ThreadDetail,
    leave_btn: &gtk::Button,
    leave_reason: &gtk::Label,
    can_leave: &Cell<bool>,
    remove_btn: &gtk::Button,
    can_remove: &Cell<bool>,
) {
    let leave = super::thread_control(detail, ThreadControl::LeaveRoom);
    let remove = super::thread_control(detail, ThreadControl::LocalRemove);
    can_leave.set(leave.is_possible());
    can_remove.set(remove.is_possible());
    crate::possible::paint(leave_btn, Some(leave_reason), leave);
    crate::possible::paint(remove_btn, None, remove);
}

/// The nest-read toggle's `checked` attribute and label, off the staged value.
fn paint_nest_read(toggle: &gtk::ToggleButton, reads: bool) {
    toggle.set_active(reads);
    toggle.set_label(if reads {
        crate::i18n::strings::conversations::unified::ROOM_NEST_READ_YES
    } else {
        crate::i18n::strings::conversations::unified::ROOM_NEST_READ_NO
    });
    crate::testid::set_test_attr(toggle, "checked", if reads { "true" } else { "false" });
}

/// A live editor: the caller keeps it to report a refusal (which leaves the
/// editor open, its refused value still staged) or the all-landed close.
pub struct RoomSettingsEditor {
    window: gtk::Window,
    error_label: gtk::Label,
    pending_box: gtk::Box,
    on_withdraw: Rc<dyn Fn(String)>,
    leave_btn: gtk::Button,
    /// The walk-out's reason line: the owner's blocked answer names the
    /// hand-over here, within eyeshot of the dead button.
    leave_reason: gtk::Label,
    /// Whether the live `LeaveRoom` answer is possible, re-asked at the
    /// confirm's open and not only at paint (a control that is not offered
    /// live must not be openable by a driver).
    can_leave: Rc<Cell<bool>>,
    /// `room-remove-button` — the shared `LocalRemove` answer: painted on a
    /// bridged room, absent elsewhere (`bridged-adapter.md` § Removing bridged
    /// rows, rule 6).
    remove_btn: gtk::Button,
    /// Whether the live `LocalRemove` answer is possible, re-asked at the
    /// confirm's open like [`Self::can_leave`].
    can_remove: Rc<Cell<bool>>,
    /// Held so the section's weak self-references stay upgradable for as long
    /// as the editor lives.
    _labelers: Rc<LabelerSection>,
}

impl RoomSettingsEditor {
    /// Every staged commit landed — close (`ui/conversations.md` § Element
    /// IDs: "closes only when all landed").
    pub fn close(&self) {
        self.window.close();
    }

    /// Whether the editor's window is still up (Esc and Save-close destroy it).
    pub fn is_open(&self) -> bool {
        self.window.is_visible()
    }

    /// Repaint what stands pending from the LIVE detail. The editor is built
    /// once from the detail at open, but the home nest serves the list after a
    /// floor read and a withdrawal removes a row, so the section is rebuilt off
    /// every snapshot tick rather than frozen at open. The whole section is
    /// ABSENT while `pending_invites` is `None` (not served) or empty alike.
    pub fn sync_live(&self, detail: &ThreadDetail) {
        paint_pending(&self.pending_box, detail, &self.on_withdraw);
        paint_departures(
            detail,
            &self.leave_btn,
            &self.leave_reason,
            &self.can_leave,
            &self.remove_btn,
            &self.can_remove,
        );
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
/// `on_leave` is the walk-out (`room-leave-confirm`): one manager call, outside
/// Save's staged set. The editor closes itself first. `on_remove` is the local
/// remove (`room-remove-confirm`, `bridged-adapter.md` § Removing bridged rows,
/// rule 6): the walk-out's twin, one manager call, immediate.
///
/// `None` where the shared seed opens no editor (a non-room). A policy-less
/// room — a bridged room — opens it only for its local remove
/// (`RoomSettingsDraft::seed`), and then only the walk-out and the remove are
/// painted: it has no policy, roles, read or labelers to show.
pub fn show<
    F: Fn(RoomSettingsDraft) + 'static,
    W: Fn(String) + 'static,
    L: Fn() + 'static,
    R: Fn() + 'static,
>(
    parent: &impl IsA<gtk::Widget>,
    detail: &ThreadDetail,
    on_save: F,
    on_withdraw: W,
    on_leave: L,
    on_remove: R,
    labelers: LabelerSeams,
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
        // Every closure the window's own tree holds takes the window WEAKLY:
        // a strong capture is a cycle (window → widget → closure → window)
        // that outlives the close, and with it every painted control stays in
        // the possibility registry the walk's I9 reads.
        let window = window.downgrade();
        esc.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if let Some(window) = window.upgrade() {
                    window.close();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    window.add_controller(esc);

    let draft = Rc::new(RefCell::new(seed));
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    // Every policy control lives in `policy_area`, which is attached only
    // where the room has a policy to show; a policy-less room's editor holds
    // the departures alone.
    let has_policy = detail.room.as_ref().is_some_and(|r| r.policy.is_some());
    let policy_area = gtk::Box::new(gtk::Orientation::Vertical, 8);
    if has_policy {
        content.append(&policy_area);
    }

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
    // Every policy control is the shared `SetPolicy` answer: absent for a
    // plain member, who opens this editor to read it and to leave, and who
    // reads each value as a plain line where the control would sit
    // (`ui/conversations.md` § Errors & edge cases → *Controls this person can
    // never use*).
    let set_policy = super::thread_control(detail, ThreadControl::SetPolicy);
    let reads_only = !set_policy.is_shown();
    crate::possible::paint(&join_rule_dd, None, set_policy.clone());
    join_rule_row.append(&join_rule_dd);
    if reads_only {
        join_rule_row.append(&gtk::Label::new(Some(join_rule_label(
            draft.borrow().join_rule,
        ))));
    }
    policy_area.append(&join_rule_row);

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
    crate::possible::paint(&history_dd, None, set_policy.clone());
    history_row.append(&history_dd);
    if reads_only {
        history_row.append(&gtk::Label::new(Some(history_policy_label(
            draft.borrow().history_policy,
        ))));
    }
    policy_area.append(&history_row);

    // The per-member rows are torn down and rebuilt on every staging gesture,
    // so the `checked` attributes and the hand-over's at-most-one rule are
    // always read straight off the draft rather than tracked per widget.
    let members_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    policy_area.append(&members_box);

    // The home nest's read, only where there is one to stage: a community room
    // whose answer this device has read (`RoomSettingsDraft::nest_read`).
    // Staged like the admin switches — Save commits it as a rotation.
    // A viewer who may never stage it reads the same sentence as a line.
    if let Some(reads) = draft.borrow().nest_read
        && reads_only
    {
        let line = gtk::Label::new(Some(if reads {
            crate::i18n::strings::conversations::unified::ROOM_NEST_READ_YES
        } else {
            crate::i18n::strings::conversations::unified::ROOM_NEST_READ_NO
        }));
        line.set_halign(gtk::Align::Start);
        policy_area.append(&line);
    }
    if let Some(reads) = draft.borrow().nest_read
        && !reads_only
    {
        let toggle = gtk::ToggleButton::new();
        toggle.set_halign(gtk::Align::Start);
        crate::possible::paint(&toggle, None, set_policy.clone());
        crate::testid::set_test_id(&toggle, ids::ROOM_NEST_READ_TOGGLE);
        paint_nest_read(&toggle, reads);
        {
            let draft = Rc::clone(&draft);
            let may = set_policy.is_possible();
            toggle.connect_clicked(move |btn| {
                if !may {
                    return;
                }
                let mut d = draft.borrow_mut();
                d.toggle_nest_read();
                paint_nest_read(btn, d.nest_read.unwrap_or(false));
            });
        }
        policy_area.append(&toggle);
    }

    let on_withdraw: Rc<dyn Fn(String)> = Rc::new(on_withdraw);
    let pending_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    paint_pending(&pending_box, detail, &on_withdraw);
    policy_area.append(&pending_box);

    // The room's labeler set: staged like the rest, committed by Save. Absent
    // where the draft carries no set. Its inspect view opens in place.
    let labeler_section = LabelerSection::new(&draft, labelers, set_policy.clone());
    policy_area.append(&labeler_section.root);
    {
        let weak = Rc::downgrade(&labeler_section);
        window.connect_close_request(move |_| {
            if let Some(section) = weak.upgrade() {
                section.close();
            }
            glib::Propagation::Proceed
        });
    }

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
    let appoint = super::thread_control(detail, ThreadControl::AppointAdmins);
    let transfer = super::thread_control(detail, ThreadControl::TransferOwnership);
    // A whole per-participant list is absent for a viewer without the role
    // behind it; with neither list held, no member rows are painted at all.
    members_box.set_visible(appoint.is_shown() || transfer.is_shown());

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
                    &appoint,
                    &transfer,
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
    // Absent for a member: every staged edit behind Save is owner-or-admin
    // work.
    crate::possible::paint(&save_btn, None, set_policy.clone());
    if has_policy {
        content.append(&save_btn);
    }

    // The walk-out, below Save and outside the staged set: leaving is an
    // immediate act, not a policy edit. The shared `LeaveRoom` answer is
    // blocked for the owner — painted dead with the hand-over as its reason on
    // the line below it, the hand-over controls live above — and the answer
    // is re-asked when the confirm opens. The remove beside it is the
    // `LocalRemove` answer: absent off a bridged rail.
    let can_leave = Rc::new(Cell::new(false));
    let can_remove = Rc::new(Cell::new(false));
    let leave_btn =
        gtk::Button::with_label(crate::i18n::strings::conversations::unified::ROOM_LEAVE);
    leave_btn.set_halign(gtk::Align::End);
    crate::testid::set_test_id(&leave_btn, ids::ROOM_LEAVE_BUTTON);
    let leave_reason = gtk::Label::new(None);
    leave_reason.set_halign(gtk::Align::End);
    leave_reason.set_wrap(true);
    leave_reason.add_css_class("dim-label");
    let remove_btn =
        gtk::Button::with_label(crate::i18n::strings::conversations::unified::ROOM_REMOVE);
    remove_btn.set_halign(gtk::Align::End);
    crate::testid::set_test_id(&remove_btn, ids::ROOM_REMOVE_BUTTON);
    paint_departures(
        detail,
        &leave_btn,
        &leave_reason,
        &can_leave,
        &remove_btn,
        &can_remove,
    );
    {
        let can_leave = Rc::clone(&can_leave);
        let window = window.downgrade();
        let label = detail.label.clone();
        let on_leave = Rc::new(on_leave);
        leave_btn.connect_clicked(move |btn| {
            if !can_leave.get() {
                return;
            }
            let window = window.clone();
            let on_leave = Rc::clone(&on_leave);
            crate::confirm_dialog::present_confirm(
                btn,
                crate::confirm_dialog::ConfirmSpec::new(
                    crate::i18n::strings::conversations::unified::ROOM_LEAVE,
                    crate::confirm_dialog::ConfirmBody::Text(&label),
                    "leave",
                    crate::i18n::strings::common::LEAVE,
                    crate::confirm_dialog::CANCEL,
                )
                .with_confirm_id(ids::ROOM_LEAVE_CONFIRM)
                .with_wire_kind("fauna.conversations.room.leave"),
                move || {
                    if let Some(window) = window.upgrade() {
                        window.close();
                    }
                    on_leave();
                },
            );
        });
    }
    content.append(&leave_btn);
    content.append(&leave_reason);

    // The local remove's confirm is immediate.
    {
        let can_remove = Rc::clone(&can_remove);
        let window = window.downgrade();
        let label = detail.label.clone();
        let on_remove = Rc::new(on_remove);
        remove_btn.connect_clicked(move |btn| {
            if !can_remove.get() {
                return;
            }
            let window = window.clone();
            let on_remove = Rc::clone(&on_remove);
            crate::confirm_dialog::present_confirm(
                btn,
                crate::confirm_dialog::ConfirmSpec::new(
                    crate::i18n::strings::conversations::unified::ROOM_REMOVE,
                    crate::confirm_dialog::ConfirmBody::Text(&label),
                    "remove",
                    crate::i18n::strings::conversations::unified::ROOM_REMOVE_CONFIRM,
                    crate::confirm_dialog::CANCEL,
                )
                .with_confirm_id(ids::ROOM_REMOVE_CONFIRM)
                .with_wire_kind(fauna_protocol::bridged_conversations::KIND_ROOMS_REMOVE),
                move || {
                    if let Some(window) = window.upgrade() {
                        window.close();
                    }
                    on_remove();
                },
            );
        });
    }
    content.append(&remove_btn);

    {
        let draft = Rc::clone(&draft);
        let may = set_policy.is_possible();
        save_btn.connect_clicked(move |_| {
            if !may {
                return;
            }
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
        pending_box,
        on_withdraw,
        leave_btn,
        leave_reason,
        can_leave,
        remove_btn,
        can_remove,
        _labelers: labeler_section,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_conversations::room::RoomRole;
    use fauna_labeler_catalog_machine::{FakeLabelerCatalogNestApi, LabelerCatalogMachine};
    use std::sync::Arc;

    /// The editor over `detail`, with a catalog that serves nothing.
    fn open(detail: &ThreadDetail) -> (Rc<RoomSettingsEditor>, tokio::runtime::Runtime) {
        let rt = tokio::runtime::Runtime::new().expect("a runtime for the catalog seam");
        let machine = LabelerCatalogMachine::new(
            Arc::new(fauna_labeler_catalog_machine::observer::NullObserver),
            Arc::new(FakeLabelerCatalogNestApi::new()),
        );
        let parent = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let editor = show(
            &parent,
            detail,
            |_| {},
            |_| {},
            || {},
            || {},
            LabelerSeams {
                machine,
                rt: rt.handle().clone(),
            },
        )
        .expect("a governed room opens its editor");
        (editor, rt)
    }

    fn find(editor: &RoomSettingsEditor, id: &str) -> Option<gtk::Widget> {
        let root: gtk::Widget = editor.window.clone().upcast();
        crate::automation::find::find_in(&root, id)
    }

    fn count(editor: &RoomSettingsEditor, id: &str) -> usize {
        let root: gtk::Widget = editor.window.clone().upcast();
        let mut found = Vec::new();
        crate::automation::find::collect_in(&root, id, &mut found);
        found.len()
    }

    /// `ui/conversations.md` § Errors & edge cases → *Controls this person can
    /// never use*: a plain member reads the room's settings, with no policy
    /// control, no per-participant control and no Save, and the walk-out
    /// live; the local remove is absent off a bridged rail.
    #[test]
    fn a_plain_member_reads_the_policy_and_may_only_leave() {
        crate::testid::run_on_gtk_thread(|| {
            let detail = super::super::thread_header::tests::governed_two_seats(RoomRole::Member);
            let (editor, _rt) = open(&detail);
            for id in [
                ids::ROOM_JOIN_RULE_SELECT,
                ids::ROOM_HISTORY_POLICY_SELECT,
                ids::ROOM_ADMIN_TOGGLE,
                ids::ROOM_OWNER_TRANSFER_BUTTON,
                ids::ROOM_SETTINGS_SAVE_BUTTON,
                ids::ROOM_REMOVE_BUTTON,
            ] {
                assert!(
                    find(&editor, id).is_none(),
                    "{id} is not offered to a member"
                );
            }
            let leave = find(&editor, ids::ROOM_LEAVE_BUTTON).expect("the walk-out is painted");
            assert!(leave.is_sensitive(), "a member may walk out");
            assert!(
                !editor.leave_reason.is_visible(),
                "no reason under a live walk-out"
            );
            assert!(editor.can_leave.get() && !editor.can_remove.get());
            // The values are read as lines where the pickers would sit.
            let root: gtk::Widget = editor.window.clone().upcast();
            let mut texts = Vec::new();
            let mut stack = vec![root];
            while let Some(w) = stack.pop() {
                if let Some(label) = w.downcast_ref::<gtk::Label>() {
                    texts.push(label.text().to_string());
                }
                let mut child = w.first_child();
                while let Some(c) = child {
                    child = c.next_sibling();
                    stack.push(c);
                }
            }
            assert!(
                texts.iter().any(|t| t == join_rule_label(JoinRule::Invite)),
                "the join rule is read as a line: {texts:?}"
            );
            // Destroyed, not closed: a test window left alive keeps its painted
            // widgets in the possibility registry the walk reads after it.
            editor.window.destroy();
        });
    }

    /// The owner holds every policy control, one per-participant row per
    /// chip, and a walk-out BLOCKED with the hand-over sentence painted on its
    /// reason line — the one control of the set that is shown dead.
    #[test]
    fn the_owners_walk_out_is_blocked_with_the_hand_over_in_view() {
        crate::testid::run_on_gtk_thread(|| {
            let detail = super::super::thread_header::tests::governed_two_seats(RoomRole::Owner);
            let (editor, _rt) = open(&detail);
            assert!(find(&editor, ids::ROOM_JOIN_RULE_SELECT).is_some_and(|w| w.is_sensitive()));
            assert!(
                find(&editor, ids::ROOM_SETTINGS_SAVE_BUTTON).is_some_and(|w| w.is_sensitive())
            );
            assert_eq!(
                count(&editor, ids::ROOM_ADMIN_TOGGLE),
                2,
                "one row per chip"
            );
            assert_eq!(count(&editor, ids::ROOM_OWNER_TRANSFER_BUTTON), 2);
            let leave = find(&editor, ids::ROOM_LEAVE_BUTTON).expect("blocked, never hidden");
            assert!(!leave.is_sensitive(), "a room is never owner-less");
            assert!(editor.leave_reason.is_visible());
            assert_eq!(
                editor.leave_reason.text().as_str(),
                crate::i18n::strings::error::send::ROOM_OWNER_CANNOT_LEAVE,
                "the blocked walk-out names its remedy within eyeshot"
            );
            assert!(
                !editor.can_leave.get(),
                "and its click re-asks the same answer"
            );
            let leave_weak = editor.leave_btn.downgrade();
            drop(leave);
            editor.window.destroy();
            drop(editor);
            assert!(
                leave_weak.upgrade().is_none(),
                "a closed editor frees its controls: no closure in its tree holds the window"
            );
        });
    }
}
