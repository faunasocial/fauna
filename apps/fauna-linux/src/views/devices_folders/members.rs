//! The Devices sub-page's **member group** — signed-in devices without a
//! matching entry (`docs/goal/ui/devices.md` § Members without a matching
//! entry; `fleet-scope-reclamation.md`
//! § *Fleet-scope reclamation*, clause (4), *A disagreement is the user's to settle*).
//!
//! A separately-labelled group below the roster, never intermixed with
//! `device-card` rows, that paints NOTHING while `DevicesSnapshot::members` is
//! empty (a settled honest fleet lists nobody — the custody families' shape).
//! Per member: the shared-Rust fingerprint, the sign-in time the member itself
//! claims, and the two-step remove. This module paints; it derives nothing —
//! the list, the fingerprint and the removal door are the shared
//! `DevicesMachine`'s.
//!
//! The arm state is the armed card's fleet id (`FleetMemberSummary::device_id`),
//! never a list position: a refresh between arm and confirm that reshapes the
//! list cannot retarget the removal, and an id no longer listed disarms.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use fauna_devices_machine::{DevicesMachine, FleetMemberSummary};
use fauna_ui_ids as ids;

use crate::async_helper;
use crate::i18n::strings;
use crate::testid::set_test_id;

/// The member group's widgets plus its arm state.
pub struct MemberView {
    pub group: adw::PreferencesGroup,
    list: gtk::ListBox,
    /// The fleet id of the card whose confirm/cancel pair is showing.
    armed: RefCell<Option<String>>,
    /// The members last painted, so an arm/cancel click repaints without
    /// waiting for a machine tick.
    members: RefCell<Vec<FleetMemberSummary>>,
}

impl MemberView {
    pub fn build() -> Rc<Self> {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        let note = gtk::Label::builder()
            .label(strings::devices::MEMBER_NOTE)
            .wrap(true)
            .xalign(0.0)
            .css_classes(["dim-label"])
            .margin_bottom(6)
            .build();
        set_test_id(&note, ids::DEVICE_MEMBER_NOTE);
        // Starts hidden: an honest fleet has nothing to say here, and an
        // empty titled group reads as a feature that failed to load.
        let group = adw::PreferencesGroup::builder()
            .title(strings::devices::MEMBERS_TITLE)
            .visible(false)
            .build();
        group.add(&note);
        group.add(&list);
        Rc::new(Self {
            group,
            list,
            armed: RefCell::new(None),
            members: RefCell::new(Vec::new()),
        })
    }

    pub fn append_to(&self, content: &gtk::Box) {
        content.append(&self.group);
    }

    /// Disarm — a fresh visit to the page never opens on a pending confirm.
    pub fn disarm(self: &Rc<Self>, machine: &Arc<DevicesMachine>) {
        if self.armed.replace(None).is_some() {
            self.paint(machine);
        }
    }

    /// Repaint from the machine's current snapshot.
    pub fn update(self: &Rc<Self>, members: &[FleetMemberSummary], machine: &Arc<DevicesMachine>) {
        self.members.replace(members.to_vec());
        // An armed id that no longer lists (the removal landed, or the member
        // went away) is not carried onto whatever card is next.
        let still_listed = self
            .armed
            .borrow()
            .as_ref()
            .is_none_or(|id| members.iter().any(|m| &m.device_id == id));
        if !still_listed {
            self.armed.replace(None);
        }
        self.paint(machine);
    }

    fn paint(self: &Rc<Self>, machine: &Arc<DevicesMachine>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let members = self.members.borrow().clone();
        self.group.set_visible(!members.is_empty());
        let armed = self.armed.borrow().clone();
        for member in &members {
            let is_armed = armed.as_deref() == Some(member.device_id.as_str());
            self.list
                .append(&build_member_card(self, member, is_armed, machine));
        }
    }
}

fn build_member_card(
    view: &Rc<MemberView>,
    member: &FleetMemberSummary,
    armed: bool,
    machine: &Arc<DevicesMachine>,
) -> gtk::ListBoxRow {
    // `accessible_role(Group)` at construction — the roster card's AT-SPI
    // discoverability note applies here unchanged.
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&card, ids::DEVICE_MEMBER_CARD);

    let info = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .hexpand(true)
        .build();
    // The fingerprint is the machine's own render — never a local formatter.
    let fingerprint = gtk::Label::builder()
        .label(strings::devices::member_fingerprint(&member.fingerprint))
        .halign(gtk::Align::Start)
        .selectable(true)
        .css_classes(["heading"])
        .build();
    set_test_id(&fingerprint, ids::DEVICE_MEMBER_FINGERPRINT);
    // The member's own self-signed word about when it signed in — a hint,
    // never proof; formatted locally through the shared door.
    let enrolled_at = gtk::Label::builder()
        .label(strings::devices::member_enrolled_at(
            &fauna_core::format::format_unix_local_ms(member.enrolled_at_ms),
        ))
        .halign(gtk::Align::Start)
        .css_classes(["dim-label"])
        .build();
    set_test_id(&enrolled_at, ids::DEVICE_MEMBER_ENROLLED_AT);
    info.append(&fingerprint);
    info.append(&enrolled_at);
    card.append(&info);

    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .valign(gtk::Align::Center)
        .build();
    if armed {
        let confirm = gtk::Button::builder()
            .label(strings::devices::MEMBER_REMOVE_CONFIRM)
            .css_classes(["destructive-action"])
            .build();
        set_test_id(&confirm, ids::DEVICE_MEMBER_REMOVE_CONFIRM_BUTTON);
        let cancel = gtk::Button::with_label(strings::common::CANCEL);
        set_test_id(&cancel, ids::DEVICE_MEMBER_REMOVE_CANCEL_BUTTON);
        {
            let view = Rc::clone(view);
            let machine = Arc::clone(machine);
            let device_id = member.device_id.clone();
            confirm.connect_clicked(move |_| {
                // Disarm on dispatch; the machine refreshes (observer tick) and
                // drops the card, or paints a refusal and leaves it to retry.
                view.armed.replace(None);
                view.paint(&machine);
                let machine = Arc::clone(&machine);
                let device_id = device_id.clone();
                async_helper::run_on_tokio(
                    async move { machine.remove_member_by_id(device_id).await },
                    |_| {},
                );
            });
        }
        {
            let view = Rc::clone(view);
            let machine = Arc::clone(machine);
            cancel.connect_clicked(move |_| {
                view.armed.replace(None);
                view.paint(&machine);
            });
        }
        buttons.append(&confirm);
        buttons.append(&cancel);
    } else {
        let remove = gtk::Button::with_label(strings::common::REMOVE);
        remove.add_css_class("flat");
        set_test_id(&remove, ids::DEVICE_MEMBER_REMOVE_BUTTON);
        let view = Rc::clone(view);
        let machine = Arc::clone(machine);
        let device_id = member.device_id.clone();
        remove.connect_clicked(move |_| {
            view.armed.replace(Some(device_id.clone()));
            view.paint(&machine);
        });
        buttons.append(&remove);
    }
    card.append(&buttons);

    gtk::ListBoxRow::builder()
        .child(&card)
        .activatable(false)
        .build()
}
