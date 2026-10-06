//! Box-recovery step 4 — box-selection hub (`box-recovery.md` § Recovery UI
//! (step 4)). The Linux twin of the web reference `nest_recovery` arm
//! (`apps/fauna-web/src/routes/onboarding/+page.svelte`); the wizard branch is
//! shared Rust (`fauna-onboarding-machine`), this page is the thin GTK glue.
//!
//! One selectable `recover-box-item-{n}` row per box the admin custodies
//! (`m.recovery_boxes()`). Two production glue paths push that list in: the
//! launch entry's fetch (`main.rs::handle_launch_phase`) and, on entering this
//! page, `mod.rs::fetch_recovery_boxes` — a deployment-seed plane read
//! (`fauna.state.deployment-seeds`, pre-login, over the nest when a URL is
//! resolved and the on-device store otherwise). Picking a
//! row (`select_recovery_box`) enables the two re-provision method buttons; an
//! empty list shows `recover-box-empty-message`. Only public `nest_actor_id`s
//! cross into the client — the seed itself stays in the plane.
//!
//! ui.yaml IDs (`box-recovery.md` § Recovery UI (step 4)): `recover-box-list`,
//! `recover-box-item` (indexed), `recover-box-empty-message`,
//! `recover-method-cloud-button`, `recover-method-selfhosted-button`,
//! `recover-back-button`.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use fauna_onboarding_machine::OnboardingMachine;
use gtk::{Button, Label, Orientation};

use crate::i18n::strings::common;
use crate::i18n::strings::onboarding::recovery as strings;
use crate::testid::set_test_id;

pub fn build(m: Arc<OnboardingMachine>) -> (gtk::Box, Rc<dyn Fn()>) {
    let page = gtk::Box::new(Orientation::Vertical, 0);
    page.set_vexpand(true);
    page.set_valign(gtk::Align::Center);
    page.set_halign(gtk::Align::Center);

    let content = gtk::Box::new(Orientation::Vertical, 12);
    content.set_margin_top(32);
    content.set_margin_bottom(32);
    content.set_margin_start(48);
    content.set_margin_end(48);
    content.set_width_request(420);

    let title = Label::new(Some(strings::TITLE));
    title.add_css_class("title-1");
    title.add_css_class("fauna-accent");
    title.set_halign(gtk::Align::Start);
    content.append(&title);

    let subtitle = Label::new(Some(strings::SUBTITLE));
    subtitle.add_css_class("fauna-muted");
    subtitle.set_wrap(true);
    subtitle.set_halign(gtk::Align::Start);
    subtitle.set_margin_bottom(12);
    content.append(&subtitle);

    // Empty-state message — visible when the custody map holds no boxes (a
    // fresh device with no local account store and no reachable nest that
    // serves the map — `box-recovery.md` § The plane-era recovery floor).
    let empty_message = Label::new(Some(strings::EMPTY_MESSAGE));
    empty_message.add_css_class("fauna-muted");
    empty_message.set_wrap(true);
    empty_message.set_halign(gtk::Align::Start);
    set_test_id(&empty_message, ids::RECOVER_BOX_EMPTY_MESSAGE);
    content.append(&empty_message);

    // Box-list section: a header label + the item container. The label lives
    // OUTSIDE the container so the container can be cleared+rebuilt on change
    // without wiping the header.
    let list_label = Label::new(Some(strings::BOX_LIST_LABEL));
    list_label.add_css_class("fauna-muted");
    list_label.set_halign(gtk::Align::Start);
    content.append(&list_label);

    let list = gtk::Box::new(Orientation::Vertical, 8);
    list.set_halign(gtk::Align::Fill);
    set_test_id(&list, ids::RECOVER_BOX_LIST);
    content.append(&list);

    // Method buttons — disabled until a box is selected (mirrors the machine's
    // `require_selected_recovery_box` guard; the web `disabled={!selected}`).
    let cloud_btn = Button::with_label(strings::METHOD_CLOUD);
    cloud_btn.add_css_class("suggested-action");
    cloud_btn.add_css_class("pill");
    cloud_btn.set_hexpand(true);
    cloud_btn.set_margin_top(8);
    set_test_id(&cloud_btn, ids::RECOVER_METHOD_CLOUD_BUTTON);
    {
        let m = m.clone();
        cloud_btn.connect_clicked(move |_| {
            // Advances to vps_config in recovery mode (the shared orchestrator
            // re-provisions with the saved seed installed + re-points A/AAAA as
            // its Dns step — the drive is Task C2). Errors surface via
            // error_message() → the page-local error label on the next tick.
            let _ = m.recover_via_cloud();
        });
    }
    content.append(&cloud_btn);

    let selfhosted_btn = Button::with_label(strings::METHOD_SELFHOSTED);
    selfhosted_btn.add_css_class("pill");
    selfhosted_btn.set_hexpand(true);
    set_test_id(&selfhosted_btn, ids::RECOVER_METHOD_SELFHOSTED_BUTTON);
    {
        let m = m.clone();
        selfhosted_btn.connect_clicked(move |_| {
            let _ = m.recover_via_selfhosted();
        });
    }
    content.append(&selfhosted_btn);

    let back_btn = Button::with_label(common::BACK);
    back_btn.add_css_class("flat");
    back_btn.set_margin_top(4);
    set_test_id(&back_btn, ids::RECOVER_BACK_BUTTON);
    {
        let m = m.clone();
        back_btn.connect_clicked(move |_| {
            // came-from-identity → back() returns to handle_entry (Q2-A routes
            // the fresh-client entry through nest-connect); came-from-launch
            // stays put by design — the launch glue owns that exit. Both entries
            // exist on Linux (`recover-lost-box-button` on identity_choice,
            // `launch-recover-button` on launch_retry).
            m.back();
        });
    }
    content.append(&back_btn);

    let error_label = Label::new(None);
    error_label.set_visible(false);
    error_label.set_wrap(true);
    error_label.set_halign(gtk::Align::Start);
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    content.append(&error_label);

    page.append(&content);

    // The currently-rendered row set (box_id → button) + the last-rendered box
    // list, so we rebuild rows only when the list changes (like vps_config's
    // provider-section reseed), and re-apply the selected class every tick.
    let rows: Rc<RefCell<Vec<(String, Button)>>> = Rc::new(RefCell::new(Vec::new()));
    let last_boxes: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

    let refresh: Rc<dyn Fn()> = Rc::new({
        let m = m.clone();
        move || {
            let boxes = m.recovery_boxes();
            let selected = m.recovery_selected_nest_id();

            // Rebuild rows only when the box set changed. On empty this clears
            // every `recover-box-item-*` widget so is_visible reports false.
            if *last_boxes.borrow() != boxes {
                while let Some(child) = list.first_child() {
                    list.remove(&child);
                }
                rows.borrow_mut().clear();
                for (i, id) in boxes.iter().enumerate() {
                    let row = Button::new();
                    row.add_css_class("card");
                    row.set_hexpand(true);
                    set_test_id(&row, &format!("recover-box-item-{i}"));

                    let row_content = gtk::Box::new(Orientation::Vertical, 2);
                    row_content.set_halign(gtk::Align::Start);
                    let id_label = Label::new(Some(&fauna_core::format::short_nest_id(id)));
                    id_label.add_css_class("monospace");
                    id_label.set_halign(gtk::Align::Start);
                    row_content.append(&id_label);
                    let hint = Label::new(Some(strings::BOX_ITEM_HINT));
                    hint.add_css_class("fauna-muted");
                    hint.add_css_class("caption");
                    hint.set_halign(gtk::Align::Start);
                    row_content.append(&hint);
                    row.set_child(Some(&row_content));

                    {
                        let m = m.clone();
                        let id = id.clone();
                        row.connect_clicked(move |_| {
                            m.select_recovery_box(id.clone());
                        });
                    }
                    list.append(&row);
                    rows.borrow_mut().push((id.clone(), row));
                }
                *last_boxes.borrow_mut() = boxes.clone();
            }

            let empty = boxes.is_empty();
            empty_message.set_visible(empty);
            list_label.set_visible(!empty);
            list.set_visible(!empty);

            // Selected-row visual (parity with web's `class:selected`).
            for (id, row) in rows.borrow().iter() {
                if selected.as_deref() == Some(id.as_str()) {
                    row.add_css_class("selected");
                } else {
                    row.remove_css_class("selected");
                }
            }

            // Method buttons gated on a selection.
            let has_selection = selected.is_some();
            cloud_btn.set_sensitive(has_selection);
            selfhosted_btn.set_sensitive(has_selection);

            let msg = m.error_message().filter(|msg| !msg.is_empty());
            crate::settings::render_error_label(&error_label, msg.as_deref());
        }
    });

    (page, refresh)
}
