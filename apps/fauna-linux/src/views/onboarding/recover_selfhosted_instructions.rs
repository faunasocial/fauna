//! Box-recovery step 4 — self-hosted seed install (`box-recovery.md` § Recovery
//! UI (step 4)). The Linux twin of the web reference
//! `recover_selfhosted_instructions` arm. Shows the installer command carrying
//! `FAUNA_DEPLOYMENT_SEED` so the rebuilt box re-presents the same
//! `nest_actor_id`, a copy button, a restore CTA, and a continue button.
//!
//! The real command (the `FAUNA_DEPLOYMENT_SEED=<64-hex>` line for the selected
//! box) is resolved IN RUST by the pre-login read `mod.rs` fires on page entry
//! (`fetch_selfhosted_command` → `client::load_selfhosted_recovery_command`, the
//! shared resolver: this device's own account store joined with a cold read from
//! the resolved nest), which renders the selected box's custodied seed through
//! the one shared projection — so every app emits a byte-identical line. The
//! resolved value lands in the [`CommandCell`] this page paints from; until it
//! does (and when no source custodies the box) the page shows the pending
//! placeholder, matching the web reference.
//!
//! **The command is client-held, not machine-held**, and deliberately so: the
//! onboarding machine resolves seeds inside Rust and surfaces only public
//! `nest_actor_id`s. This page is the one sanctioned exception (the command *is*
//! the installer input the admin pastes — `box-recovery.md` § Trust & audience),
//! so it is carried beside the machine rather than added to its UniFFI surface.
//!
//! It must never show a command carrying the **wrong** box's seed: that rebuilds
//! the box under a different `nest_actor_id`, which every TOFU-pinned client then
//! rejects — the exact trust break recovery exists to prevent. Hence: placeholder
//! until resolved, and the copy button disabled while it is one.
//!
//! ui.yaml IDs: `recover-selfhosted-command`, `recover-selfhosted-copy-button`,
//! `recover-selfhosted-continue-button`, `recover-restore-cta`.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use fauna_onboarding_machine::OnboardingMachine;
use gtk::{Button, Label, Orientation, gdk};

use crate::i18n::strings::common;
use crate::i18n::strings::onboarding::recovery as strings;
use crate::testid::set_test_id;

/// The resolved installer command for the selected box, shared between the
/// page (which paints it) and `mod.rs`'s page-entry read (which fills it).
/// `None` until the read lands ⇒ the pending placeholder.
pub type CommandCell = Rc<RefCell<Option<String>>>;

pub fn build(m: Arc<OnboardingMachine>, command: CommandCell) -> (gtk::Box, Rc<dyn Fn()>) {
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

    let title = Label::new(Some(strings::SELFHOSTED_TITLE));
    title.add_css_class("title-1");
    title.add_css_class("fauna-accent");
    title.set_halign(gtk::Align::Start);
    content.append(&title);

    let desc = Label::new(Some(strings::SELFHOSTED_DESC));
    desc.add_css_class("fauna-muted");
    desc.set_wrap(true);
    desc.set_halign(gtk::Align::Start);
    desc.set_margin_bottom(8);
    content.append(&desc);

    let command_label = Label::new(Some(strings::SELFHOSTED_COMMAND_PENDING));
    command_label.add_css_class("monospace");
    command_label.set_wrap(true);
    command_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    command_label.set_selectable(true);
    command_label.set_halign(gtk::Align::Start);
    set_test_id(&command_label, ids::RECOVER_SELFHOSTED_COMMAND);
    content.append(&command_label);

    let copy_btn = Button::with_label(common::COPY);
    copy_btn.add_css_class("flat");
    copy_btn.set_halign(gtk::Align::Start);
    copy_btn.set_sensitive(false);
    set_test_id(&copy_btn, ids::RECOVER_SELFHOSTED_COPY_BUTTON);
    {
        // Copy the RESOLVED command only — never the placeholder. The cell is the
        // single source both the label and the clipboard read, so the two can
        // never disagree about what the admin is pasting into their box.
        let command = command.clone();
        copy_btn.connect_clicked(move |_| {
            if let Some(text) = command.borrow().as_deref()
                && let Some(display) = gdk::Display::default()
            {
                display.clipboard().set_text(text);
            }
        });
    }
    content.append(&copy_btn);

    let button_row = gtk::Box::new(Orientation::Horizontal, 8);
    button_row.set_halign(gtk::Align::Fill);
    button_row.set_margin_top(12);

    let restore_cta = Button::with_label(strings::RESTORE_CTA);
    restore_cta.add_css_class("pill");
    restore_cta.set_hexpand(true);
    set_test_id(&restore_cta, ids::RECOVER_RESTORE_CTA);
    {
        let m = m.clone();
        restore_cta.connect_clicked(move |_| {
            // Web deep-links to the Backups page's restore-* section. On Linux
            // the Backups page lives in the authenticated app stack (app.rs),
            // unreachable from the pre-auth wizard; and the fresh-client
            // recovery path (Linux's only entry today) has no box up yet. So
            // this exits the recovery flow; the surviving-device "return to the
            // app → Backups restore" deep-link belongs to the launch-entry leg
            // (Task E launch entry, gated with C2). The box reconnects on next
            // launch once the admin has run the installer and it is reachable.
            m.reset();
        });
    }
    button_row.append(&restore_cta);

    let continue_btn = Button::with_label(strings::SELFHOSTED_CONTINUE);
    continue_btn.add_css_class("suggested-action");
    continue_btn.add_css_class("pill");
    continue_btn.set_hexpand(true);
    set_test_id(&continue_btn, ids::RECOVER_SELFHOSTED_CONTINUE_BUTTON);
    {
        let m = m.clone();
        continue_btn.connect_clicked(move |_| {
            // Exits the recovery flow (see the restore-cta note); the rebuilt
            // box reconnects via the normal launch flow once reachable.
            m.reset();
        });
    }
    button_row.append(&continue_btn);

    content.append(&button_row);

    page.append(&content);

    // Paint from the cell: the resolved installer line once the page-entry read
    // lands, the pending placeholder until then. The copy button follows it — a
    // placeholder is not something to put on the admin's clipboard.
    let refresh: Rc<dyn Fn()> = Rc::new({
        let command_label = command_label.clone();
        let copy_btn = copy_btn.clone();
        move || match command.borrow().as_deref() {
            Some(text) => {
                command_label.set_text(text);
                copy_btn.set_sensitive(true);
            }
            None => {
                command_label.set_text(strings::SELFHOSTED_COMMAND_PENDING);
                copy_btn.set_sensitive(false);
            }
        }
    });

    (page, refresh)
}
